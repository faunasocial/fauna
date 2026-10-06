//! Where the GTK main thread's time goes: per-source busy accounting for the
//! main loop, the witness that names a DEFAULT-priority saturator by
//! measurement rather than inference (`apps/linux.md` § Message Flow).
//!
//! glib dispatches only the highest ready priority, so a source that is ready
//! on every iteration starves everything beneath it — the e2e `barrier` ack
//! (an idle callback, `DEFAULT_IDLE`) and the frame clock that maps a freshly
//! presented dialog (`GDK_PRIORITY_REDRAW`) — while the e2e heartbeat, itself a
//! DEFAULT timeout, keeps beating. The heartbeat tells a stalled thread from a
//! running one (`e2e-conventions.md` convention 11); this module tells what the
//! running thread is spending itself on.
//!
//! Three parts:
//!
//! - **Meters.** A main-loop dispatch wraps its body in [`dispatch`] (one unit
//!   of work) or [`drain`] (a tick that consumes a queue, item by item). Time
//!   is recorded per source and EXCLUSIVE of nested metered work, so the
//!   sources sum to the metered total instead of double-counting.
//! - **The poll clock.** The default context's poll function is wrapped, so
//!   the time the thread spends waiting in `poll()` is known. Everything else
//!   is busy, and busy minus the metered sum is the *unmetered* remainder — the
//!   check that the meters cover the saturator at all.
//! - **Two probes.** A `DEFAULT_IDLE` callback, re-sent each second, times
//!   how long idle work waits — the exact priority the barrier ack runs at.
//!   Its twin sits one step above the frame clock (`GDK_PRIORITY_REDRAW`), so
//!   only DEFAULT-level work can hold it back: the idle probe waiting while
//!   the twin does not puts the saturator in the redraw band, not at DEFAULT.
//!   The frame clock's own paint cycles are metered as `gtk-frame`.
//!
//! A window in which the thread was mostly busy, or the probe waited, logs one
//! `[main-loop]` line ranking the sources by exclusive busy time. Only
//! [`start`] turns any of this on (the e2e automation launch calls it); until
//! then every meter is a no-op past one thread-local read.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Length of one reporting window.
const WINDOW: Duration = Duration::from_secs(5);
/// A window whose busy share reaches this logs (numerator / denominator).
const REPORT_BUSY_SHARE: (u32, u32) = (1, 2);
/// A window whose idle probe waited this long logs, at `warn`.
const REPORT_IDLE_WAIT: Duration = Duration::from_millis(500);
/// Sources named per line, heaviest first; the rest fold into "others".
const REPORT_TOP_SOURCES: usize = 8;
/// Item kinds named per draining source, heaviest first.
const REPORT_TOP_KINDS: usize = 3;

// ---------------------------------------------------------------------------
// The ledger — pure accounting, no glib, so it is unit-testable.
// ---------------------------------------------------------------------------

/// One source's totals over one window.
#[derive(Debug, Default, Clone, PartialEq)]
struct SourceWindow {
    /// Exclusive busy time: this source's own work, nested meters excluded.
    busy: Duration,
    /// Dispatches (one per [`dispatch`] / [`drain`] call).
    dispatches: u64,
    /// Items consumed across every [`drain`] of this source.
    items: u64,
    /// The longest single dispatch (inclusive).
    max_dispatch: Duration,
    /// The most items one [`drain`] consumed.
    max_items: u64,
    /// The slowest single item or dispatch, and what it was.
    slowest: Duration,
    slowest_label: Option<String>,
    /// Every drained item's kind: how many, and their summed time.
    kinds: Vec<Kind>,
}

/// One item kind's tally: its label, count and summed (inclusive) time.
type Kind = (String, u64, Duration);

fn tally(kinds: &mut Vec<Kind>, label: String, n: u64, took: Duration) {
    match kinds.iter_mut().find(|(k, _, _)| *k == label) {
        Some(entry) => {
            entry.1 += n;
            entry.2 += took;
        }
        None => kinds.push((label, n, took)),
    }
}

/// One finished measurement, as handed to [`Ledger::close`].
struct Closed {
    source: &'static str,
    inclusive: Duration,
    items: u64,
    slowest: Duration,
    slowest_label: Option<String>,
    kinds: Vec<Kind>,
}

#[derive(Default)]
struct Ledger {
    sources: Vec<(&'static str, SourceWindow)>,
    /// Open measurements, innermost last; each accumulates the inclusive time
    /// of the metered work nested inside it.
    open: Vec<Duration>,
}

impl Ledger {
    fn open(&mut self) {
        self.open.push(Duration::ZERO);
    }

    fn close(&mut self, closed: Closed) {
        let nested = self.open.pop().unwrap_or_default();
        if let Some(parent) = self.open.last_mut() {
            *parent += closed.inclusive;
        }
        let exclusive = closed.inclusive.saturating_sub(nested);
        let w = self.source(closed.source);
        w.busy += exclusive;
        w.dispatches += 1;
        w.items += closed.items;
        w.max_dispatch = w.max_dispatch.max(closed.inclusive);
        w.max_items = w.max_items.max(closed.items);
        if closed.slowest >= w.slowest {
            w.slowest = closed.slowest;
            w.slowest_label = closed.slowest_label;
        }
        for (label, n, took) in closed.kinds {
            tally(&mut w.kinds, label, n, took);
        }
    }

    fn source(&mut self, name: &'static str) -> &mut SourceWindow {
        let at = match self.sources.iter().position(|(n, _)| *n == name) {
            Some(at) => at,
            None => {
                self.sources.push((name, SourceWindow::default()));
                self.sources.len() - 1
            }
        };
        &mut self.sources[at].1
    }

    #[cfg(test)]
    fn metered(&self) -> Duration {
        self.sources.iter().map(|(_, w)| w.busy).sum()
    }

    /// Hand back this window's totals and start the next one empty. Open
    /// measurements stay open: they close into the next window.
    fn take_window(&mut self) -> Vec<(&'static str, SourceWindow)> {
        std::mem::take(&mut self.sources)
    }
}

/// What one window measured, and the report decision over it.
struct WindowReport {
    wall: Duration,
    /// Wall time minus time spent waiting in `poll()`. `None` when the poll
    /// clock could not be installed.
    busy: Option<Duration>,
    /// Longest wait of the `DEFAULT_IDLE` probe — what the barrier ack sees.
    idle_wait: Duration,
    /// Longest wait of the probe just above the frame clock
    /// ([`ABOVE_REDRAW`]). Starved only by DEFAULT (and `HIGH_IDLE`) work, so
    /// an idle probe that waits while this one does not puts the saturator in
    /// the redraw band, not at DEFAULT.
    above_redraw_wait: Duration,
    sources: Vec<(&'static str, SourceWindow)>,
}

impl WindowReport {
    fn metered(&self) -> Duration {
        self.sources.iter().map(|(_, w)| w.busy).sum()
    }

    /// The thread's busy share reached [`REPORT_BUSY_SHARE`], or the idle
    /// probe waited [`REPORT_IDLE_WAIT`]. Without a poll clock, the metered
    /// total stands in for busy (a lower bound).
    fn worth_reporting(&self) -> bool {
        let busy = self.busy.unwrap_or_else(|| self.metered());
        let (num, den) = REPORT_BUSY_SHARE;
        busy * den >= self.wall * num || self.idle_wait >= REPORT_IDLE_WAIT
    }

    fn starved(&self) -> bool {
        self.idle_wait >= REPORT_IDLE_WAIT
    }

    fn line(&self) -> String {
        let mut out = String::new();
        let wall = self.wall.as_secs_f64();
        match self.busy {
            Some(busy) => {
                let share = if wall > 0.0 {
                    100.0 * busy.as_secs_f64() / wall
                } else {
                    0.0
                };
                let _ = write!(
                    out,
                    "[main-loop] {wall:.1}s window: busy {:.2}s ({share:.0}%)",
                    busy.as_secs_f64()
                );
            }
            None => {
                let _ = write!(
                    out,
                    "[main-loop] {wall:.1}s window: busy unknown (no poll clock)"
                );
            }
        }
        let _ = write!(
            out,
            ", idle probe waited {}ms (above-redraw {}ms);",
            self.idle_wait.as_millis(),
            self.above_redraw_wait.as_millis()
        );

        let mut ranked: Vec<&(&'static str, SourceWindow)> = self.sources.iter().collect();
        ranked.sort_by_key(|a| std::cmp::Reverse(a.1.busy));
        for (name, w) in ranked.iter().take(REPORT_TOP_SOURCES) {
            let _ = write!(
                out,
                " {name} {}ms/{} (max {}ms",
                w.busy.as_millis(),
                w.dispatches,
                w.max_dispatch.as_millis()
            );
            if w.items > 0 {
                let _ = write!(out, ", {} items, max {}/tick", w.items, w.max_items);
            }
            if let Some(label) = w.slowest_label.as_ref().filter(|l| !l.is_empty()) {
                let _ = write!(out, ", slowest {label} {}ms", w.slowest.as_millis());
            }
            if !w.kinds.is_empty() {
                let mut kinds: Vec<&Kind> = w.kinds.iter().collect();
                kinds.sort_by_key(|a| std::cmp::Reverse(a.2));
                out.push_str(", kinds");
                for (label, n, took) in kinds.iter().take(REPORT_TOP_KINDS) {
                    let _ = write!(out, " {label} {n}x{}ms", took.as_millis());
                }
            }
            out.push_str(");");
        }
        if ranked.len() > REPORT_TOP_SOURCES {
            let rest: Duration = ranked[REPORT_TOP_SOURCES..]
                .iter()
                .map(|(_, w)| w.busy)
                .sum();
            let _ = write!(
                out,
                " {} others {}ms;",
                ranked.len() - REPORT_TOP_SOURCES,
                rest.as_millis()
            );
        }
        if let Some(busy) = self.busy {
            let _ = write!(
                out,
                " unmetered {}ms",
                busy.saturating_sub(self.metered()).as_millis()
            );
        }
        out
    }
}

// ---------------------------------------------------------------------------
// The thread-local meter the dispatch sites call.
// ---------------------------------------------------------------------------

/// One latency probe: a callback at a fixed priority, re-sent once the last
/// one has run.
#[derive(Default)]
struct Probe {
    /// When the outstanding probe was sent; `None` while none is out.
    sent: Option<Instant>,
    /// Longest wait a probe saw this window.
    max: Duration,
}

impl Probe {
    /// This window's longest wait, counting a probe still outstanding, and
    /// reset for the next window.
    fn take_window(&mut self, now: Instant) -> Duration {
        let outstanding = self.sent.map(|s| now.duration_since(s)).unwrap_or_default();
        std::mem::take(&mut self.max).max(outstanding)
    }
}

/// `GDK_PRIORITY_REDRAW` is `G_PRIORITY_HIGH_IDLE + 20` (120), the frame
/// clock's priority; one step above it.
const ABOVE_REDRAW: i32 = glib::ffi::G_PRIORITY_HIGH_IDLE + 19;

struct Meter {
    ledger: Ledger,
    window_start: Instant,
    poll_at_window_start: Option<u64>,
    idle: Probe,
    above_redraw: Probe,
}

thread_local! {
    static METER: RefCell<Option<Meter>> = const { RefCell::new(None) };
}

fn enabled() -> bool {
    METER.with(|m| m.borrow().is_some())
}

/// Closes its measurement on drop, so a panicking body cannot leave the
/// ledger's open stack one frame deep for the rest of the process.
struct Open {
    source: &'static str,
    start: Instant,
    items: u64,
    slowest: Duration,
    slowest_label: Option<String>,
    kinds: Vec<Kind>,
}

impl Open {
    fn begin(source: &'static str) -> Option<Self> {
        let opened = METER.with(|m| match m.borrow_mut().as_mut() {
            Some(meter) => {
                meter.ledger.open();
                true
            }
            None => false,
        });
        opened.then(|| Open {
            source,
            start: Instant::now(),
            items: 0,
            slowest: Duration::ZERO,
            slowest_label: None,
            kinds: Vec::new(),
        })
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        let closed = Closed {
            source: self.source,
            inclusive: self.start.elapsed(),
            items: self.items,
            slowest: self.slowest,
            slowest_label: self.slowest_label.take(),
            kinds: std::mem::take(&mut self.kinds),
        };
        METER.with(|m| {
            if let Some(meter) = m.borrow_mut().as_mut() {
                meter.ledger.close(closed);
            }
        });
    }
}

/// Meter one unit of main-loop work under `source`. `label` names it if it
/// turns out the slowest of its window, and is only evaluated when metering
/// is on.
pub fn dispatch<T>(
    source: &'static str,
    label: impl FnOnce() -> String,
    f: impl FnOnce() -> T,
) -> T {
    let Some(mut open) = Open::begin(source) else {
        return f();
    };
    let out = f();
    open.slowest = open.start.elapsed();
    open.slowest_label = Some(label());
    drop(open);
    out
}

/// The source name for one call site of a shared helper — `kind@file:line` —
/// so every wake loop (or render, or completion) is booked under its own
/// name instead of folding into one: a single loop that never goes quiet is
/// exactly what an aggregate hides. Interned once per site; the set of sites
/// is fixed at compile time, so the leaked names are bounded.
pub fn site_source(
    kind: &'static str,
    caller: &'static std::panic::Location<'static>,
) -> &'static str {
    type Site = (&'static str, &'static str, u32, &'static str);
    thread_local! {
        static SITES: RefCell<Vec<Site>> = const { RefCell::new(Vec::new()) };
    }
    let (file, line) = (caller.file(), caller.line());
    SITES.with_borrow_mut(|sites| {
        if let Some(&(_, _, _, name)) = sites
            .iter()
            .find(|(k, f, l, _)| *k == kind && *f == file && *l == line)
        {
            return name;
        }
        let short = file.strip_prefix("apps/fauna-linux/src/").unwrap_or(file);
        let name: &'static str = Box::leak(format!("{kind}@{short}:{line}").into_boxed_str());
        sites.push((kind, file, line, name));
        name
    })
}

/// [`dispatch`] for a body that is not a closure — a loop pass inside an async
/// task. The measurement closes when the guard drops; drop it BEFORE the
/// task's next `.await`, or the wait is charged as busy and every dispatch
/// that runs meanwhile is booked as nested inside this one.
pub struct DispatchGuard(#[allow(dead_code)] Option<Open>);

/// Open a [`DispatchGuard`] under `source`.
pub fn dispatch_guard(source: &'static str) -> DispatchGuard {
    DispatchGuard(Open::begin(source))
}

/// One [`drain`] tick in progress: meter each consumed item through
/// [`Drain::item`].
pub struct Drain {
    open: Option<Open>,
}

impl Drain {
    /// Run one item of the tick. `label` is its kind: tallied per window, and
    /// named as the slowest item if it is. Evaluated only when metering is on.
    pub fn item<T>(&mut self, label: impl FnOnce() -> String, f: impl FnOnce() -> T) -> T {
        let Some(open) = self.open.as_mut() else {
            return f();
        };
        let label = label();
        let start = Instant::now();
        let out = f();
        let took = start.elapsed();
        open.items += 1;
        if took >= open.slowest {
            open.slowest = took;
            open.slowest_label = Some(label.clone());
        }
        tally(&mut open.kinds, label, 1, took);
        out
    }
}

/// Meter one queue-draining tick under `source`.
pub fn drain<T>(source: &'static str, f: impl FnOnce(&mut Drain) -> T) -> T {
    let mut tick = Drain {
        open: Open::begin(source),
    };
    f(&mut tick)
}

/// The variant path of an enum value, off its `Debug` form, without
/// formatting the payload: `Data(FeedLoaded { … })` reads `Data::FeedLoaded`.
/// The writer refuses everything past a short prefix, so a message carrying a
/// fifty-post payload costs no more to name than an empty one.
pub fn variant_path(value: &impl std::fmt::Debug) -> String {
    struct Prefix(String);
    impl std::fmt::Write for Prefix {
        fn write_str(&mut self, s: &str) -> std::fmt::Result {
            let room = 64usize.saturating_sub(self.0.len());
            if room == 0 {
                return Err(std::fmt::Error);
            }
            let mut end = room.min(s.len());
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            self.0.push_str(&s[..end]);
            if end < s.len() {
                Err(std::fmt::Error)
            } else {
                Ok(())
            }
        }
    }
    let mut prefix = Prefix(String::new());
    let _ = write!(prefix, "{value:?}");
    prefix
        .0
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|seg| !seg.is_empty())
        .take(2)
        .collect::<Vec<_>>()
        .join("::")
}

// ---------------------------------------------------------------------------
// The poll clock.
// ---------------------------------------------------------------------------

type PollFn = unsafe extern "C" fn(
    *mut glib::ffi::GPollFD,
    std::ffi::c_uint,
    std::ffi::c_int,
) -> std::ffi::c_int;

/// Nanoseconds the default context has spent inside `poll()` since install.
static POLL_NANOS: AtomicU64 = AtomicU64::new(0);
static ORIGINAL_POLL: OnceLock<PollFn> = OnceLock::new();

unsafe extern "C" fn metered_poll(
    fds: *mut glib::ffi::GPollFD,
    nfds: std::ffi::c_uint,
    timeout: std::ffi::c_int,
) -> std::ffi::c_int {
    let Some(original) = ORIGINAL_POLL.get() else {
        return -1;
    };
    let start = Instant::now();
    // SAFETY: forwards glib's own arguments, unchanged, to the poll function
    // glib had installed on this context before `install_poll_clock` replaced it.
    let ret = unsafe { original(fds, nfds, timeout) };
    POLL_NANOS.fetch_add(start.elapsed().as_nanos() as u64, Ordering::Relaxed);
    ret
}

/// Wrap the default main context's poll function in [`metered_poll`]. Returns
/// whether the clock is running (it is installed at most once per process).
fn install_poll_clock() -> bool {
    if ORIGINAL_POLL.get().is_some() {
        return true;
    }
    // SAFETY: both calls take the process-default context, which glib owns for
    // the life of the process; the replacement forwards to the original.
    unsafe {
        let ctx = glib::ffi::g_main_context_default();
        let Some(original) = glib::ffi::g_main_context_get_poll_func(ctx) else {
            return false;
        };
        if ORIGINAL_POLL.set(original).is_err() {
            return true;
        }
        glib::ffi::g_main_context_set_poll_func(ctx, Some(metered_poll));
    }
    true
}

fn poll_nanos() -> Option<u64> {
    ORIGINAL_POLL
        .get()
        .map(|_| POLL_NANOS.load(Ordering::Relaxed))
}

// ---------------------------------------------------------------------------
// Start + the reporting tick.
// ---------------------------------------------------------------------------

/// Turn the meter on for this (the GTK main) thread: install the poll clock,
/// and start the one-second tick that re-sends both probes, hooks any new
/// toplevel's frame clock, and closes each window. Idempotent.
pub fn start() {
    if enabled() {
        return;
    }
    let clock = install_poll_clock();
    METER.with(|m| {
        *m.borrow_mut() = Some(Meter {
            ledger: Ledger::default(),
            window_start: Instant::now(),
            poll_at_window_start: if clock { poll_nanos() } else { None },
            idle: Probe::default(),
            above_redraw: Probe::default(),
        });
    });
    glib::timeout_add_local(Duration::from_secs(1), || {
        send_probe(glib::Priority::DEFAULT_IDLE, |m| &mut m.idle);
        send_probe(glib::Priority::from(ABOVE_REDRAW), |m| &mut m.above_redraw);
        hook_frame_clocks();
        close_window_if_due();
        glib::ControlFlow::Continue
    });
}

fn send_probe(priority: glib::Priority, probe: fn(&mut Meter) -> &mut Probe) {
    let sent = METER.with(|m| {
        let mut m = m.borrow_mut();
        let p = probe(m.as_mut()?);
        if p.sent.is_some() {
            return None; // still waiting: the window close reads its age
        }
        let now = Instant::now();
        p.sent = Some(now);
        Some(now)
    });
    if let Some(sent) = sent {
        glib::idle_add_local_full(priority, move || {
            let waited = sent.elapsed();
            METER.with(|m| {
                if let Some(meter) = m.borrow_mut().as_mut() {
                    let p = probe(meter);
                    p.max = p.max.max(waited);
                    p.sent = None;
                }
            });
            glib::ControlFlow::Break
        });
    }
}

thread_local! {
    /// Frame clocks already hooked, so a toplevel is hooked once.
    static HOOKED_CLOCKS: RefCell<Vec<glib::WeakRef<gtk::gdk::FrameClock>>> =
        const { RefCell::new(Vec::new()) };
    /// The paint cycle in progress, opened at `before-paint`.
    static PAINTING: RefCell<Option<DispatchGuard>> = const { RefCell::new(None) };
}

/// Meter every realized toplevel's frame clock as `gtk-frame`: one dispatch
/// per paint cycle, `before-paint` to `after-paint` — the update, layout and
/// paint phases GTK runs at `GDK_PRIORITY_REDRAW`.
fn hook_frame_clocks() {
    use gtk::prelude::*;
    let toplevels = gtk::Window::toplevels();
    for i in 0..toplevels.n_items() {
        let Some(clock) = toplevels
            .item(i)
            .and_then(|o| o.downcast::<gtk::Widget>().ok())
            .and_then(|w| w.frame_clock())
        else {
            continue;
        };
        let fresh = HOOKED_CLOCKS.with_borrow_mut(|hooked| {
            hooked.retain(|w| w.upgrade().is_some());
            if hooked.iter().any(|w| w.upgrade().as_ref() == Some(&clock)) {
                return false;
            }
            hooked.push(clock.downgrade());
            true
        });
        if !fresh {
            continue;
        }
        clock.connect_before_paint(|_| {
            PAINTING.with_borrow_mut(|p| *p = Some(dispatch_guard("gtk-frame")));
        });
        clock.connect_after_paint(|_| {
            PAINTING.with_borrow_mut(|p| p.take());
        });
    }
}

fn close_window_if_due() {
    let report = METER.with(|m| {
        let mut m = m.borrow_mut();
        let meter = m.as_mut()?;
        let now = Instant::now();
        let wall = now.duration_since(meter.window_start);
        if wall < WINDOW {
            return None;
        }
        let poll_now = poll_nanos();
        let busy = match (meter.poll_at_window_start, poll_now) {
            (Some(then), Some(now_ns)) => {
                Some(wall.saturating_sub(Duration::from_nanos(now_ns.saturating_sub(then))))
            }
            _ => None,
        };
        let report = WindowReport {
            wall,
            busy,
            idle_wait: meter.idle.take_window(now),
            above_redraw_wait: meter.above_redraw.take_window(now),
            sources: meter.ledger.take_window(),
        };
        meter.window_start = now;
        meter.poll_at_window_start = poll_now;
        Some(report)
    });
    if let Some(report) = report
        && report.worth_reporting()
    {
        // `info`, not `debug`: the e2e launch runs at the default `info`
        // filter, and this line exists to be read in exactly that log.
        if report.starved() {
            tracing::warn!("{}", report.line());
        } else {
            tracing::info!("{}", report.line());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn closed(source: &'static str, inclusive: Duration, items: u64) -> Closed {
        Closed {
            source,
            inclusive,
            items,
            slowest: inclusive,
            slowest_label: Some(format!("{source}-item")),
            kinds: vec![(format!("{source}-item"), items, inclusive)],
        }
    }

    /// A metered dispatch nested inside another is charged to itself only: the
    /// outer source keeps its exclusive time, and the two sum to the outer's
    /// inclusive time rather than counting the inner twice.
    #[test]
    fn nested_work_is_charged_to_the_innermost_source_only() {
        let mut ledger = Ledger::default();
        ledger.open(); // outer
        ledger.open(); // inner
        ledger.close(closed("inner", ms(30), 0));
        ledger.close(closed("outer", ms(100), 0));
        let window = ledger.take_window();
        let busy = |name| window.iter().find(|(n, _)| *n == name).unwrap().1.busy;
        assert_eq!(busy("outer"), ms(70));
        assert_eq!(busy("inner"), ms(30));
    }

    #[test]
    fn a_source_accumulates_dispatches_items_and_its_worst_tick() {
        let mut ledger = Ledger::default();
        for (took, items) in [(ms(10), 3), (ms(40), 12), (ms(5), 1)] {
            ledger.open();
            ledger.close(closed("ui-pump", took, items));
        }
        assert_eq!(ledger.metered(), ms(55));
        let window = ledger.take_window();
        let (_, w) = &window[0];
        assert_eq!(w.dispatches, 3);
        assert_eq!(w.items, 16);
        assert_eq!(w.max_items, 12);
        assert_eq!(w.max_dispatch, ms(40));
        assert!(
            ledger.take_window().is_empty(),
            "take_window starts the next one empty"
        );
    }

    fn report(busy: Option<Duration>, idle_wait: Duration) -> WindowReport {
        WindowReport {
            wall: ms(5000),
            busy,
            idle_wait,
            above_redraw_wait: Duration::ZERO,
            sources: vec![],
        }
    }

    #[test]
    fn a_quiet_window_is_not_reported_and_a_busy_or_starved_one_is() {
        assert!(!report(Some(ms(400)), ms(3)).worth_reporting());
        assert!(report(Some(ms(2500)), ms(3)).worth_reporting());
        assert!(report(Some(ms(400)), ms(900)).worth_reporting());
        assert!(report(Some(ms(400)), ms(900)).starved());
        assert!(!report(Some(ms(4900)), ms(3)).starved());
    }

    /// The line ranks sources heaviest first and reports what the meters did
    /// not cover, so a saturator outside every meter still shows up.
    #[test]
    fn the_line_ranks_sources_and_names_the_unmetered_remainder() {
        let mut ledger = Ledger::default();
        for (source, took) in [("wake-loop", ms(200)), ("agent-state-publish", ms(3000))] {
            ledger.open();
            ledger.close(closed(source, took, 0));
        }
        let line = WindowReport {
            wall: ms(5000),
            busy: Some(ms(4000)),
            idle_wait: ms(1200),
            above_redraw_wait: ms(40),
            sources: ledger.take_window(),
        }
        .line();
        let heavy = line.find("agent-state-publish").expect("heaviest named");
        let light = line.find("wake-loop").expect("lighter named");
        assert!(heavy < light, "ranked heaviest first: {line}");
        assert!(line.contains("unmetered 800ms"), "{line}");
        assert!(
            line.contains("idle probe waited 1200ms (above-redraw 40ms)"),
            "{line}"
        );
    }

    /// A draining source's items are tallied by kind across its ticks, and
    /// the line names the kinds that cost the most — which is what tells a
    /// burst of one message from a mix.
    #[test]
    fn drained_items_are_tallied_by_kind_across_ticks() {
        let mut ledger = Ledger::default();
        for kinds in [
            vec![
                ("Realtime::Disconnected".to_string(), 1, ms(300)),
                ("Data::FeedLoaded".to_string(), 4, ms(40)),
            ],
            vec![("Realtime::Disconnected".to_string(), 2, ms(500))],
        ] {
            ledger.open();
            ledger.close(Closed {
                source: "ui-pump",
                inclusive: kinds.iter().map(|k| k.2).sum(),
                items: kinds.iter().map(|k| k.1).sum(),
                slowest: ms(0),
                slowest_label: None,
                kinds,
            });
        }
        let line = WindowReport {
            wall: ms(5000),
            busy: Some(ms(900)),
            idle_wait: ms(0),
            above_redraw_wait: ms(0),
            sources: ledger.take_window(),
        }
        .line();
        assert!(
            line.contains("kinds Realtime::Disconnected 3x800ms Data::FeedLoaded 4x40ms"),
            "{line}"
        );
    }

    /// Each call site of a shared helper is its own source, named once.
    #[test]
    fn a_call_site_is_its_own_source_interned_once() {
        let here = std::panic::Location::caller();
        let a = site_source("wake-loop", here);
        let b = site_source("wake-loop", here);
        assert!(std::ptr::eq(a, b), "interned: one name per site");
        assert!(
            a.starts_with("wake-loop@") && a.ends_with(&format!(":{}", here.line())),
            "{a}"
        );
        assert_ne!(
            site_source("snapshot-render", here),
            a,
            "the kind is part of the key"
        );
    }

    #[derive(Debug)]
    #[allow(dead_code)]
    enum Inner {
        FeedLoaded { posts: Vec<String> },
    }
    #[derive(Debug)]
    #[allow(dead_code)]
    enum Outer {
        Data(Inner),
        Noop,
    }

    #[test]
    fn variant_path_names_the_variant_without_the_payload() {
        let big = Outer::Data(Inner::FeedLoaded {
            posts: vec!["x".repeat(10_000); 50],
        });
        assert_eq!(variant_path(&big), "Data::FeedLoaded");
        assert_eq!(variant_path(&Outer::Noop), "Noop");
    }
}
