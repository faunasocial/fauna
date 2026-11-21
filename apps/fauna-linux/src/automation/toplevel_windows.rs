//! The journal behind `fauna_e2e_agent::TOPLEVEL_WINDOWS_KEY` (which owns the
//! contract): every change of the set of visible top-level windows, recorded
//! from the windows' own notifications so a gap shorter than a frame is still
//! an entry. The bookkeeping (dedupe, cap, `seq`, the JSON shape) is the
//! shared `fauna_e2e_agent::ToplevelWindowJournal`; this module is only the
//! GTK half that feeds it.
//!
//! Beside it, the journal behind `fauna_e2e_agent::ROOT_SURFACE_KEY` (which
//! owns that contract): what the app's root showed, fed from every hooked
//! window's frame clock once per painted frame and at every visibility change.
//! The root is the newest visible window that names one
//! (`window_host::set_root_surface`); no visible such window is a frame that
//! painted nothing.
//!
//! Main-thread only, like everything in this module. Installed once, when the
//! element agent starts (`start_element_agent_once`), before the first window
//! is shown.

use fauna_e2e_agent::ToplevelWindowJournal;
use gtk::glib;
use gtk::prelude::*;
use serde_json::Value;
use std::cell::RefCell;

/// One visible window: its serial and its widget name.
type Seen = (u64, String);

#[derive(Default)]
struct Journal {
    /// Every window this process has hooked, with its serial.
    windows: Vec<(glib::WeakRef<gtk::Window>, u64)>,
    next_id: u64,
    /// The recorded sets — the shared journal owns dedupe, cap and `seq`.
    journal: ToplevelWindowJournal,
}

thread_local! {
    static JOURNAL: RefCell<Journal> = RefCell::new(Journal::default());
    static ROOT_SURFACE: RefCell<fauna_e2e_agent::RootSurfaceJournal> =
        RefCell::new(fauna_e2e_agent::RootSurfaceJournal::default());
    static INSTALLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Hook the toplevel list and every window on it. Idempotent.
pub fn install() {
    if INSTALLED.replace(true) {
        return;
    }
    let toplevels = gtk::Window::toplevels();
    toplevels.connect_items_changed(|_, _, _, _| {
        hook_new_windows();
        record();
    });
    hook_new_windows();
    record();
}

fn hook_new_windows() {
    let toplevels = gtk::Window::toplevels();
    for i in 0..toplevels.n_items() {
        let Some(win) = toplevels.item(i).and_downcast::<gtk::Window>() else {
            continue;
        };
        let known = JOURNAL.with_borrow_mut(|j| {
            j.windows.retain(|(w, _)| w.upgrade().is_some());
            if j.windows
                .iter()
                .any(|(w, _)| w.upgrade().as_ref() == Some(&win))
            {
                return true;
            }
            j.next_id += 1;
            let id = j.next_id;
            j.windows.push((win.downgrade(), id));
            false
        });
        if !known {
            win.connect_visible_notify(|_| record());
            win.connect_name_notify(|_| record());
            // A window gets a frame clock when it is realized (and a new one
            // if it is realized again); each painted frame feeds the
            // root-surface journal.
            fn hook_clock(win: &gtk::Window) {
                if let Some(clock) = win.frame_clock() {
                    clock.connect_after_paint(|_| observe_root_surface());
                }
            }
            if win.is_realized() {
                hook_clock(&win);
            }
            win.connect_realize(hook_clock);
        }
    }
}

/// The visible windows now, in serial order.
fn visible_now() -> Vec<Seen> {
    JOURNAL.with_borrow(|j| {
        let mut out: Vec<Seen> = j
            .windows
            .iter()
            .filter_map(|(w, id)| w.upgrade().map(|w| (w, *id)))
            .filter(|(w, _)| w.is_visible())
            .map(|(w, id)| (id, w.widget_name().to_string()))
            .collect();
        out.sort_by_key(|(id, _)| *id);
        out
    })
}

fn record() {
    observe_root_surface();
    observe_toplevels();
}

/// Feed the visible windows now to the toplevel journal (an entry only when
/// the set changed).
fn observe_toplevels() {
    let now = visible_now();
    let pairs: Vec<(u64, &str)> = now.iter().map(|(id, name)| (*id, name.as_str())).collect();
    JOURNAL.with_borrow_mut(|j| j.journal.observe(&pairs));
}

/// What the root shows now: the newest visible window's name for it, or
/// `None` when no visible window names one.
fn root_surface_now() -> Option<&'static str> {
    let windows: Vec<gtk::Window> = JOURNAL.with_borrow(|j| {
        let mut live: Vec<(u64, gtk::Window)> = j
            .windows
            .iter()
            .filter_map(|(w, id)| w.upgrade().map(|w| (*id, w)))
            .collect();
        live.sort_by_key(|(id, _)| std::cmp::Reverse(*id));
        live.into_iter().map(|(_, w)| w).collect()
    });
    windows
        .iter()
        .filter(|w| w.is_visible())
        .find_map(crate::window_host::root_surface_of)
}

/// Record what the root shows now into the root-surface journal (an entry only
/// when it changed). A no-op until [`install`] ran.
pub fn observe_root_surface() {
    if !INSTALLED.get() {
        return;
    }
    let showing = root_surface_now();
    ROOT_SURFACE.with_borrow_mut(|j| j.observe(showing));
}

/// The `root_surface` state value.
pub fn root_surface_json() -> Value {
    ROOT_SURFACE.with_borrow(|j| j.json())
}

/// The `toplevel_windows` state value.
pub fn toplevel_windows_json() -> Value {
    // Observe once more so `now` is read at query time (a no-op when unchanged).
    observe_toplevels();
    JOURNAL.with_borrow(|j| j.journal.json())
}
