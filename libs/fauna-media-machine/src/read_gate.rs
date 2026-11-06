//! The Media page's **read gate**: at most one aggregate read in flight, and
//! at most one trailing re-read for every refresh asked while it ran.
//!
//! The nest fires one `SyncChanged` per recorded file, and every app re-reads
//! the visible Media page on each, so a folder sync used to start hundreds of
//! overlapping full reads — each of them a roster read per set, all competing
//! for the same connection — and on a large account no read landed before the
//! next started (`docs/goal/ui/README.md` § List pages: loading is not empty).
//! The gate lives in the shared machine so all seven apps inherit it and none
//! debounces on its own.
//!
//! The contract a caller keeps: `refresh().await` returns only after a read
//! that **started after the call** has committed — so a gesture's re-read
//! still shows its own write. A refresh asked while a read runs waits for the
//! read after it, which every refresh asked meanwhile shares.
//!
//! Runtime-agnostic (the machine runs native and in the browser): a std mutex
//! and a waker list, no executor of its own.

use std::future::Future;
use std::sync::Mutex;
use std::task::{Context, Poll, Waker};

/// What a caller's turn at the gate came to.
pub(crate) enum Turn {
    /// A read that started after this caller's ask has committed (and, for a
    /// caller that needs the settle, a settle covering it has ended).
    Done,
    /// No read is running: this caller runs the next one. It must call
    /// [`ReadGate::release`] when done — [`RunGuard`] does, even on a drop.
    Run,
}

#[derive(Default)]
struct Inner {
    /// Asks so far; each refresh takes the next number.
    asked: u64,
    /// The highest ask a committed read covered (a read covers every ask
    /// made before it started).
    finished: u64,
    /// The highest ask an ended settle covered.
    settled: u64,
    /// The highest ask that needs the settle to end before it returns (an
    /// upload resolving its target), 0 for none.
    force_upto: u64,
    /// A caller is running reads.
    running: bool,
    wakers: Vec<Waker>,
}

impl Inner {
    fn wake_all(&mut self) {
        for w in self.wakers.drain(..) {
            w.wake();
        }
    }
}

#[derive(Default)]
pub(crate) struct ReadGate {
    inner: Mutex<Inner>,
}

impl ReadGate {
    /// Take a ticket for a refresh: the ask number its read must cover.
    /// `force` = the caller needs the settle covering it to END (complete, or
    /// its budget spent), not just one read.
    pub(crate) fn ask(&self, force: bool) -> u64 {
        let mut g = self.inner.lock().unwrap();
        g.asked += 1;
        let ticket = g.asked;
        if force {
            g.force_upto = ticket;
        }
        // A runner sleeping between settle re-reads re-reads at once.
        g.wake_all();
        ticket
    }

    /// Wait for this ticket's turn: done, or the runner's role.
    pub(crate) fn turn(&self, ticket: u64, force: bool) -> impl Future<Output = Turn> + '_ {
        std::future::poll_fn(move |cx| {
            let mut g = self.inner.lock().unwrap();
            let covered = if force {
                g.settled >= ticket
            } else {
                g.finished >= ticket
            };
            if covered {
                return Poll::Ready(Turn::Done);
            }
            if !g.running {
                g.running = true;
                return Poll::Ready(Turn::Run);
            }
            g.wakers.push(cx.waker().clone());
            Poll::Pending
        })
    }

    /// The runner starts a read: the asks it covers (every one made so far),
    /// and whether one of them needs the settle.
    pub(crate) fn begin_read(&self) -> (u64, bool) {
        let g = self.inner.lock().unwrap();
        (g.asked, g.force_upto > g.settled)
    }

    /// The runner's read covering `cover` has committed.
    pub(crate) fn finish_read(&self, cover: u64) {
        let mut g = self.inner.lock().unwrap();
        g.finished = g.finished.max(cover);
        g.wake_all();
    }

    /// The runner's settle has ended, its last read covering `cover`.
    pub(crate) fn end_settle(&self, cover: u64) {
        let mut g = self.inner.lock().unwrap();
        g.finished = g.finished.max(cover);
        g.settled = g.settled.max(cover);
        g.wake_all();
    }

    /// Whether an ask arrived after the read covering `cover` started.
    pub(crate) fn asked_since(&self, cover: u64) -> bool {
        self.inner.lock().unwrap().asked > cover
    }

    /// Resolves once an ask arrives after `cover` — what cuts a settle's
    /// back-off short, so a gesture's re-read never waits out a sleep.
    pub(crate) fn ask_after(&self, cover: u64) -> impl Future<Output = ()> + '_ {
        std::future::poll_fn(move |cx| {
            let mut g = self.inner.lock().unwrap();
            if g.asked > cover {
                return Poll::Ready(());
            }
            g.wakers.push(cx.waker().clone());
            Poll::Pending
        })
    }

    fn release(&self) {
        let mut g = self.inner.lock().unwrap();
        g.running = false;
        g.wake_all();
    }
}

/// The runner's role, released when dropped — a runner whose future is
/// dropped mid-read (an app cancelling its task) must not leave the gate
/// closed for ever.
pub(crate) struct RunGuard<'a>(pub(crate) &'a ReadGate);

impl Drop for RunGuard<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// Run `a` and `b` together; whichever finishes first wins and the other is
/// dropped. The two futures here (a sleep and [`ReadGate::ask_after`]) are
/// both cancel-safe.
pub(crate) async fn first_of<A, B>(a: A, b: B)
where
    A: Future<Output = ()>,
    B: Future<Output = ()>,
{
    let mut a = Box::pin(a);
    let mut b = Box::pin(b);
    std::future::poll_fn(move |cx: &mut Context<'_>| {
        if a.as_mut().poll(cx).is_ready() || b.as_mut().poll(cx).is_ready() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
    .await
}
