//! Engagement-cue viewport observer for the Feed post list
//! (`docs/goal/behavior/engagement-cues.md` § Cue vocabulary & derivation).
//!
//! **This module is a geometry probe and nothing else.** It measures where each
//! post card sits, when to sample, and when the list goes away; every piece of
//! bookkeeping above that — visibility bucketing, dwell credit and its stall
//! cap, the hold-vs-leave policy, the single-sample noise floor, `is_media`
//! stamping, [`CueObservation`] assembly — belongs to the shared
//! [`fauna_feed::CueTracker`], and all derivation below that to
//! `fauna_feed::CueEngine`. Linux is Rust-native, so it constructs the tracker
//! directly (no FFI); the other shells reach the same type through
//! `FfiCueTracker` / `WasmCueTracker`.
//!
//! *(Until 2026-07-29 this file also held the bookkeeping, and windows, android
//! and apple each held a hand-written copy of it. Two of the four had already
//! drifted onto the wall clock for dwell credit. The boundary moved one layer
//! down; this is what is left.)*
//!
//! Mechanics: a [`CUE_SAMPLE_INTERVAL_MS`] tick (plus an extra sample on every
//! scroll-position change) walks the `post-card` rows, reads each one's bounds
//! against the retained `ScrolledWindow`, and hands the tracker one
//! [`CueRow`] per row plus the loaded window's post ids. Rows are destroyed +
//! rebuilt on every manager notify (`render_posts`), so identity comes from the
//! row's own `post` test-attr — never an index-into-`snapshot.posts` read, which
//! mid-rebuild attributes one post's dwell to another. A row that cannot be
//! measured is reported with a non-positive height (or omitted); the tracker
//! holds it rather than treating it as gone.
//!
//! Playback progress comes from the inline player: a media row's
//! `media_played_pm` is what `views::feed::take_played_pm`
//! hands over for its post (the peak since the last sample), so a video played
//! to the end derives the media `watch-complete`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use gtk::glib;
use gtk::prelude::*;
use tokio::runtime::Handle;

use fauna_core::scoring::cues::CUE_SAMPLE_INTERVAL_MS;
use fauna_feed::{CueObservation, CueRow, CueTracker, LeaveModel};

use super::host::LinuxFeedManager;

/// The shared tracker plus this wire-up's monotonic epoch. `Instant` has no
/// absolute reading, so the probe converts to the tracker's `mono_now_ms` by
/// measuring from a zero captured when the observer was wired — a per-session
/// origin is all the tracker needs, since it only ever takes differences.
struct Probe {
    tracker: CueTracker,
    mono_epoch: Instant,
}

impl Probe {
    fn new() -> Self {
        Probe {
            // GTK's `ListBox` keeps a widget for every row regardless of scroll
            // position, so an unmeasurable row is mid-rebuild, never disposed.
            tracker: CueTracker::new(LeaveModel::HoldUnmeasured),
            mono_epoch: Instant::now(),
        }
    }

    fn mono_now_ms(&self) -> u64 {
        self.mono_epoch.elapsed().as_millis() as u64
    }
}

/// Wall-clock milliseconds — the observation stamp only, never dwell credit.
fn wall_now_ms() -> u64 {
    fauna_core::data::Timestamp::now_millis_or_zero()
}

/// Wire the observer onto the feed post list. Called once per auth from
/// `build_post_list_wired` (the view — and the manager it samples — are
/// rebuilt together on re-auth).
pub fn wire(
    scrolled: &gtk::ScrolledWindow,
    list: &gtk::ListBox,
    manager: &Arc<LinuxFeedManager>,
    rt: &Handle,
    error_label: &gtk::Label,
) {
    let probe = Rc::new(RefCell::new(Probe::new()));
    let ticking = Rc::new(Cell::new(false));

    // The close path's drain (`drain_for_close`): the latest wiring is the
    // live feed view, exactly as for the test clock below.
    {
        let probe = Rc::clone(&probe);
        let manager = Arc::clone(manager);
        let rt = rt.clone();
        let error_label = error_label.clone();
        LIVE_DRAIN.with(|d| {
            *d.borrow_mut() = Some(Box::new(move || {
                flush_all(&probe, &manager, &rt, &error_label)
            }))
        });
    }

    #[cfg(any(debug_assertions, feature = "e2e-agent"))]
    test_clock::register(&probe, scrolled, list, manager, rt, error_label);

    // Extra sample on every scroll movement, so a card's leave-the-viewport
    // edge is caught at scroll time rather than up to one tick later.
    {
        let probe = Rc::clone(&probe);
        let scrolled_w = scrolled.clone();
        let list = list.clone();
        let manager = Arc::clone(manager);
        let rt = rt.clone();
        let error_label = error_label.clone();
        scrolled.vadjustment().connect_value_changed(move |_| {
            sample(&probe, &scrolled_w, &list, &manager, &rt, &error_label);
        });
    }

    // The tick runs only while the list is mapped; unmapping (detail view,
    // page switch, window close) flushes every tracked card — off-screen is
    // off-viewport — and stops the source until the next map.
    let scrolled_w = scrolled.clone();
    let list = list.clone();
    let manager = Arc::clone(manager);
    let rt = rt.clone();
    let error_label = error_label.clone();
    scrolled.connect_map(move |_| {
        if ticking.get() {
            return;
        }
        ticking.set(true);
        let probe = Rc::clone(&probe);
        let ticking = Rc::clone(&ticking);
        let scrolled = scrolled_w.clone();
        let list = list.clone();
        let manager = Arc::clone(&manager);
        let rt = rt.clone();
        let error_label = error_label.clone();
        glib::timeout_add_local(Duration::from_millis(CUE_SAMPLE_INTERVAL_MS), move || {
            if !scrolled.is_mapped() {
                flush_all(&probe, &manager, &rt, &error_label);
                ticking.set(false);
                return glib::ControlFlow::Break;
            }
            crate::main_loop_meter::dispatch("feed-cue-sampler", String::new, || {
                sample(&probe, &scrolled, &list, &manager, &rt, &error_label)
            });
            glib::ControlFlow::Continue
        });
    });
}

thread_local! {
    /// The most recently wired capture's drain — one wiring per auth, so the
    /// latest is the live feed view. Main-thread only, like the probe.
    static LIVE_DRAIN: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
}

/// Exposures handed to [`emit`] whose `record_observation` has not finished.
static EMITS_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

fn emits_settled_notify() -> &'static tokio::sync::Notify {
    static NOTIFY: OnceLock<tokio::sync::Notify> = OnceLock::new();
    NOTIFY.get_or_init(tokio::sync::Notify::new)
}

/// The close path's first half (engagement-cues.md § At rest: put on batch
/// **or on background/close**; tui's `flush_on_quit` is the twin): drain every
/// exposure still in progress into the engine. The window closing is the card
/// leaving the viewport, but the tick that would notice the unmap may never
/// run again once the main loop winds down — so without this, a dwell still
/// on screen at quit was never folded, and the flush that follows had nothing
/// to put. Main thread only (a no-op elsewhere, where no capture is wired).
pub fn drain_for_close() {
    LIVE_DRAIN.with(|d| {
        if let Some(drain) = d.borrow().as_ref() {
            drain();
        }
    });
}

/// The close path's second half: resolves once every exposure [`emit`] has
/// handed off is folded (its `record_observation` returned). A leave just
/// before quit spawns its records on the runtime; flushing before they fold
/// would put a rollup without them. The caller bounds the wait.
pub async fn emits_settled() {
    loop {
        let notified = emits_settled_notify().notified();
        if EMITS_IN_FLIGHT.load(Ordering::Acquire) == 0 {
            return;
        }
        notified.await;
    }
}

/// Decrements [`EMITS_IN_FLIGHT`] however the emit task ends.
struct EmitInFlight;

impl Drop for EmitInFlight {
    fn drop(&mut self) {
        EMITS_IN_FLIGHT.fetch_sub(1, Ordering::AcqRel);
        emits_settled_notify().notify_waiters();
    }
}

/// One probe read: collect every row's geometry, hand it to the shared tracker,
/// and emit whatever exposures it says have finished.
fn sample(
    probe: &Rc<RefCell<Probe>>,
    scrolled: &gtk::ScrolledWindow,
    list: &gtk::ListBox,
    manager: &Arc<LinuxFeedManager>,
    rt: &Handle,
    error_label: &gtk::Label,
) {
    let posts = manager.snapshot().posts;
    let viewport_end = f64::from(scrolled.height());

    let mut rows: Vec<CueRow> = Vec::new();
    let mut child = list.first_child();
    while let Some(w) = child {
        child = w.next_sibling();
        let Ok(row) = w.downcast::<gtk::ListBoxRow>() else {
            continue;
        };
        let Some(post_id) = row_post_id(&row) else {
            continue;
        };
        // No bounds at all this sample: omit the row entirely — the tracker
        // reads that the same way it reads a non-positive height (held, not
        // gone, as long as the post is still in the loaded window).
        let Some(bounds) = row.compute_bounds(scrolled) else {
            continue;
        };
        let is_media = posts
            .iter()
            .find(|p| p.post_id == post_id)
            .map(|p| p.plays_media)
            .unwrap_or(false);
        // A row `render_posts` has just rebuilt has no allocation until the
        // next frame's layout phase, yet `compute_bounds` still answers with
        // its bare CSS border box at a stale origin (measured: every row at
        // `top -428, height 5` mid-rebuild) — a fabricated reading that put
        // the dwelled card "out of view" and ended its exposure early. Under
        // load the layout phase lags the rebuild further, so more samples
        // land in that gap. The allocation is the truth: an unallocated row
        // is reported unmeasurable (height 0), which the tracker holds.
        let height = if row.height() > 0 {
            f64::from(bounds.height())
        } else {
            0.0
        };
        rows.push(CueRow {
            media_played_pm: if is_media {
                crate::views::feed::take_played_pm(&post_id)
            } else {
                None
            },
            post_id,
            top: f64::from(bounds.y()),
            // A not-yet-allocated row reports a non-positive height; deciding
            // what that means is the tracker's job, not the probe's.
            height,
            is_media,
        });
    }

    let window_post_ids: Vec<String> = posts.iter().map(|p| p.post_id.clone()).collect();
    let mut p = probe.borrow_mut();
    let mono_now_ms = p.mono_now_ms();
    let left = p.tracker.sample(
        &rows,
        &window_post_ids,
        0.0,
        viewport_end,
        mono_now_ms,
        wall_now_ms(),
    );
    drop(p);
    emit(left, manager, rt, error_label);
}

/// The row's own post identity, stamped by `build_post_card` as the `post`
/// test-attr (a `test-attr-post-<id>` CSS class).
fn row_post_id(row: &gtk::ListBoxRow) -> Option<String> {
    row.css_classes()
        .iter()
        .find_map(|c| c.strip_prefix("test-attr-post-").map(str::to_string))
}

/// Emit every tracked card (page unmapped — everything left the viewport).
fn flush_all(
    probe: &Rc<RefCell<Probe>>,
    manager: &Arc<LinuxFeedManager>,
    rt: &Handle,
    error_label: &gtk::Label,
) {
    let left = probe.borrow_mut().tracker.drain_all(wall_now_ms());
    emit(left, manager, rt, error_label);
}

/// Hand the finished exposures to the shared engine
/// (`FeedManager::record_observation`); a failure surfaces on the page
/// `error-message` (the `dispatch_train` idiom). The tracker has already
/// applied the single-sample noise floor, so everything here is a real exposure.
fn emit(
    left: Vec<CueObservation>,
    manager: &Arc<LinuxFeedManager>,
    rt: &Handle,
    error_label: &gtk::Label,
) {
    if left.is_empty() {
        return;
    }
    let m = Arc::clone(manager);
    let error_label = error_label.clone();
    EMITS_IN_FLIGHT.fetch_add(1, Ordering::AcqRel);
    let in_flight = EmitInFlight;
    crate::async_helper::spawn_with_snapshot(
        rt,
        move || async move {
            let _in_flight = in_flight;
            for obs in left {
                if let Err(msg) = m.record_observation(obs).await {
                    return Some(msg);
                }
            }
            None
        },
        move |err: Option<String>| {
            if let Some(msg) = err {
                crate::settings::render_error_label(&error_label, Some(&msg));
            }
        },
    );
}

/// The engagement-cue test clock's linux doors (convention 14). The frozen
/// clock itself is the shared tracker's (`fauna_feed::CueTracker`, § *The test
/// clock*); this keeps a handle on the most recently wired capture — one per
/// auth, so the latest wiring is the live feed view — for the four
/// `feed_cue_clock_*` / `feed_cue_probe` agent arms in `main.rs`, tui's twins.
///
/// linux samples on a timer, not per draw, so two advances could otherwise
/// fold into one sample's capped credit: [`test_clock::advance`] samples
/// right after it moves the clock, and its ack is that sample's barrier. A
/// scroll lands in the rows' geometry only at the next frame's layout, so the
/// frozen probe and the pixel step ([`test_clock::scroll_by`]) answer after
/// that frame, from a sample of their own.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub mod test_clock {
    use super::*;

    struct Live {
        probe: Rc<RefCell<Probe>>,
        resample: Rc<dyn Fn()>,
        scrolled: gtk::ScrolledWindow,
    }

    /// A deferred command's answer — the arm acks inside it.
    pub type Reply = Box<dyn FnOnce(Result<serde_json::Value, String>)>;

    thread_local! {
        static LIVE: RefCell<Option<Live>> = const { RefCell::new(None) };
    }

    pub(super) fn register(
        probe: &Rc<RefCell<Probe>>,
        scrolled: &gtk::ScrolledWindow,
        list: &gtk::ListBox,
        manager: &Arc<LinuxFeedManager>,
        rt: &Handle,
        error_label: &gtk::Label,
    ) {
        let resample = {
            let probe = Rc::clone(probe);
            let scrolled = scrolled.clone();
            let list = list.clone();
            let manager = Arc::clone(manager);
            let rt = rt.clone();
            let error_label = error_label.clone();
            Rc::new(move || sample(&probe, &scrolled, &list, &manager, &rt, &error_label))
        };
        LIVE.with(|l| {
            *l.borrow_mut() = Some(Live {
                probe: Rc::clone(probe),
                resample,
                scrolled: scrolled.clone(),
            })
        });
    }

    fn with_live<T>(f: impl FnOnce(&Live) -> T) -> Result<T, String> {
        LIVE.with(|l| {
            l.borrow()
                .as_ref()
                .map(f)
                .ok_or_else(|| "no feed cue capture is wired (pre-auth)".to_string())
        })
    }

    /// Freeze both clocks at their current real readings. Idempotent.
    pub fn freeze() -> Result<(), String> {
        with_live(|live| {
            let mut p = live.probe.borrow_mut();
            let mono = p.mono_now_ms();
            p.tracker.freeze_clock_for_test(mono, wall_now_ms());
        })
    }

    /// Move both frozen clocks forward by `ms`, then sample at once (while the
    /// list is mapped) so the step lands as exactly one sample's credit.
    pub fn advance(ms: u64) -> Result<(), String> {
        with_live(|live| {
            live.probe.borrow_mut().tracker.advance_clock_for_test(ms)?;
            if live.scrolled.is_mapped() {
                (live.resample)();
            }
            Ok(())
        })?
    }

    /// Back to the real clocks.
    pub fn thaw() -> Result<(), String> {
        with_live(|live| live.probe.borrow_mut().tracker.thaw_clock_for_test())
    }

    fn read(probe: &Rc<RefCell<Probe>>) -> serde_json::Value {
        serde_json::to_value(probe.borrow().tracker.probe_for_test())
            .unwrap_or(serde_json::Value::Null)
    }

    /// Run `f` once the next frame has laid the list out — a tick callback
    /// fires before that frame's layout, and an idle queued from it runs after
    /// the whole frame.
    fn after_next_frame(scrolled: &gtk::ScrolledWindow, f: impl FnOnce() + 'static) {
        let f = RefCell::new(Some(f));
        scrolled.add_tick_callback(move |_, _| {
            if let Some(f) = f.borrow_mut().take() {
                glib::idle_add_local_once(f);
            }
            glib::ControlFlow::Break
        });
    }

    /// The shared probe (`fauna_feed::CueProbe`), in the `ScrolledWindow`'s
    /// pixel space. Frozen and on screen, it answers after the next frame from
    /// a sample of its own — a frozen sample credits nothing, it only
    /// measures — so it reads the list as laid out, not as the last tick saw it.
    pub fn probe(reply: Reply) {
        let live = with_live(|live| {
            (
                Rc::clone(&live.probe),
                Rc::clone(&live.resample),
                live.scrolled.clone(),
            )
        });
        let (probe, resample, scrolled) = match live {
            Ok(live) => live,
            Err(e) => return reply(Err(e)),
        };
        if !probe.borrow().tracker.clock_frozen_for_test() || !scrolled.is_mapped() {
            return reply(Ok(read(&probe)));
        }
        after_next_frame(&scrolled, move || {
            resample();
            reply(Ok(read(&probe)));
        });
    }

    /// The journeys' pixel step: scroll the post list by `px` (the real
    /// adjustment the user's scroll moves), then answer after the next frame
    /// from a sample of its own, with the probe as THAT sample left it — a
    /// later read could already show the feed's re-rank in answer to an
    /// exposure the step ended. `moved: false` at the end of the list.
    pub fn scroll_by(px: f64, reply: Reply) {
        let live = with_live(|live| {
            (
                Rc::clone(&live.probe),
                Rc::clone(&live.resample),
                live.scrolled.clone(),
            )
        });
        let (probe, resample, scrolled) = match live {
            Ok(live) => live,
            Err(e) => return reply(Err(e)),
        };
        if !scrolled.is_mapped() {
            return reply(Err("the feed list is not on screen".to_string()));
        }
        let adj = scrolled.vadjustment();
        let before = adj.value();
        adj.set_value(before + px);
        let moved = adj.value() != before;
        after_next_frame(&scrolled, move || {
            resample();
            reply(Ok(
                serde_json::json!({ "ok": true, "moved": moved, "probe": read(&probe) }),
            ));
        });
    }
}
