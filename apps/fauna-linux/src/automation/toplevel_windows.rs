//! The journal behind `fauna_e2e_agent::TOPLEVEL_WINDOWS_KEY` (which owns the
//! contract): every change of the set of visible top-level windows, recorded
//! from the windows' own notifications so a gap shorter than a frame is still
//! an entry.
//!
//! Main-thread only, like everything in this module. Installed once, when the
//! element agent starts (`start_element_agent_once`), before the first window
//! is shown.

use gtk::glib;
use gtk::prelude::*;
use serde_json::{Value, json};
use std::cell::RefCell;

/// One visible window: its serial and its widget name.
type Seen = (u64, String);

#[derive(Default)]
struct Journal {
    /// Every window this process has hooked, with its serial.
    windows: Vec<(glib::WeakRef<gtk::Window>, u64)>,
    next_id: u64,
    /// The recorded sets, oldest first; consecutive entries always differ.
    entries: Vec<Vec<Seen>>,
}

/// Entries kept: a long run keeps the newest, which hold every transition a
/// witness reads (the window set moves a handful of times per journey).
const MAX_ENTRIES: usize = 512;

thread_local! {
    static JOURNAL: RefCell<Journal> = RefCell::new(Journal::default());
}

/// Hook the toplevel list and every window on it. Idempotent.
pub fn install() {
    thread_local! {
        static INSTALLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
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
    let now = visible_now();
    JOURNAL.with_borrow_mut(|j| {
        if j.entries.last() != Some(&now) {
            j.entries.push(now);
            if j.entries.len() > MAX_ENTRIES {
                let drop = j.entries.len() - MAX_ENTRIES;
                j.entries.drain(..drop);
            }
        }
    });
}

fn seen_json(set: &[Seen]) -> Value {
    Value::Array(
        set.iter()
            .map(|(id, name)| json!({"id": id, "name": name}))
            .collect(),
    )
}

/// The `toplevel_windows` state value.
pub fn toplevel_windows_json() -> Value {
    let now = visible_now();
    JOURNAL.with_borrow(|j| {
        let journal: Vec<Value> = j
            .entries
            .iter()
            .enumerate()
            .map(|(seq, set)| json!({"seq": seq, "visible": seen_json(set)}))
            .collect();
        json!({"journal": journal, "now": seen_json(&now)})
    })
}
