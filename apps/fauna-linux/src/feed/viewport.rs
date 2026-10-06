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
//! Linux renders feed media as still images only (no playback surface), so
//! `media_played_pm` is always `None` here: the media watch-complete gate can
//! never fire on this client — only the dwell-derived non-media gate and the
//! skip gate can (a video-capable client exercises the playback path).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
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
            .map(|p| p.has_media)
            .unwrap_or(false);
        rows.push(CueRow {
            post_id,
            top: f64::from(bounds.y()),
            // A not-yet-allocated row reports its non-positive height as-is;
            // deciding what that means is the tracker's job, not the probe's.
            height: f64::from(bounds.height()),
            is_media,
            // Still images only on linux — no playback surface to read.
            media_played_pm: None,
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
    crate::async_helper::spawn_with_snapshot(
        rt,
        move || async move {
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
