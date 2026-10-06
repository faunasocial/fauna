//! The two window-claim observables the connection-gap journeys rest on —
//! linux's legs of `fauna_e2e_agent::CONNECTION_REPORTS_KEY` and
//! `fauna_e2e_agent::PAINTED_ERRORS_KEY` (both contracts, the counting and the
//! JSON live in the shared crate; tui's `App` fields are the reference).
//!
//! **Connection reports** are fed where the indicator takes its word
//! (`app.rs`'s `update_connection_indicator`), every report counted — the
//! offline gate's own `set_connection_state` early-returns on a repeat, which
//! is exactly the report the stickiness proof needs, so it is counted before
//! that gate, never inside it.
//!
//! **Painted errors** are fed without walking the whole tree per frame. Every
//! error surface gets its id through [`crate::testid::set_test_id`], which
//! registers it here; each registered surface's `map`, `unmap` and (for a
//! label) `notify::label` schedules one observation at `HIGH_IDLE` — ahead of
//! GDK's redraw priority, so the observation lands before the frame that
//! paints the change and a surface raised and cleared within one paint cycle
//! (never painted) is never counted. "Painted" is `is_mapped()`: a mapped
//! widget is drawn, a crossfading page's outgoing error included. The state
//! tick observes too, as the backstop for a non-label surface whose text lives
//! in descendant labels (no signal of its own fires on their change).
use fauna_e2e_agent::{ConnectionReports, PaintedErrorTally};
use gtk::glib;
use gtk::prelude::*;
use serde_json::Value;
use std::cell::{Cell, RefCell};

thread_local! {
    static CONNECTION_REPORTS: RefCell<ConnectionReports> = RefCell::new(ConnectionReports::default());
    static PAINTED_ERRORS: RefCell<PaintedErrorTally> = RefCell::new(PaintedErrorTally::default());
    static ERROR_SURFACES: RefCell<Vec<glib::WeakRef<gtk::Widget>>> = const { RefCell::new(Vec::new()) };
    static OBSERVE_QUEUED: Cell<bool> = const { Cell::new(false) };
}

/// Count one connection-state report of `word` (a `ConnectionState` wire word).
pub fn observe_connection_report(word: &'static str) {
    CONNECTION_REPORTS.with_borrow_mut(|r| r.observe(word));
}

/// The `connection_reports` state value.
pub fn connection_reports_json() -> Value {
    CONNECTION_REPORTS.with_borrow(ConnectionReports::json)
}

/// Register a widget that has just been given the error-surface id `id`
/// (`fauna_e2e_agent::is_error_surface`), so its appearance, disappearance and
/// text changes are observed. Idempotent per widget.
pub fn register_error_surface(widget: &gtk::Widget) {
    let known = ERROR_SURFACES.with_borrow_mut(|surfaces| {
        surfaces.retain(|w| w.upgrade().is_some());
        if surfaces
            .iter()
            .any(|w| w.upgrade().as_ref() == Some(widget))
        {
            return true;
        }
        surfaces.push(widget.downgrade());
        false
    });
    if known {
        return;
    }
    widget.connect_map(|_| schedule_observation());
    widget.connect_unmap(|_| schedule_observation());
    if let Some(label) = widget.downcast_ref::<gtk::Label>() {
        label.connect_label_notify(|_| schedule_observation());
    }
    schedule_observation();
}

fn schedule_observation() {
    if OBSERVE_QUEUED.replace(true) {
        return;
    }
    glib::idle_add_local_full(glib::Priority::HIGH_IDLE, || {
        OBSERVE_QUEUED.set(false);
        observe_painted_errors();
        glib::ControlFlow::Break
    });
}

/// Observe the current frame's painted error surfaces now.
pub fn observe_painted_errors() {
    let frame: Vec<(String, String)> = ERROR_SURFACES.with_borrow_mut(|surfaces| {
        surfaces.retain(|w| w.upgrade().is_some());
        surfaces
            .iter()
            .filter_map(|w| w.upgrade())
            .filter(|w| w.is_mapped())
            .map(|w| (w.widget_name().to_string(), super::find::text_of(&w)))
            .collect()
    });
    PAINTED_ERRORS.with_borrow_mut(|tally| {
        tally.observe(frame.iter().map(|(id, text)| (id.as_str(), text.as_str())));
    });
}

/// The `painted_errors` state value.
pub fn painted_errors_json() -> Value {
    PAINTED_ERRORS.with_borrow(PaintedErrorTally::json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testid::run_on_gtk_thread;

    /// A registered error label counts once when it paints, not again while it
    /// stands, and again when raised a second time — and an unmapped (never
    /// painted) one never counts.
    #[test]
    fn a_registered_error_label_counts_once_per_appearance() {
        run_on_gtk_thread(|| {
            let window = gtk::Window::new();
            let label = gtk::Label::new(Some("boom"));
            crate::testid::set_test_id(&label, "compose-error");
            label.set_visible(false);
            window.set_child(Some(&label));
            window.present();
            observe_painted_errors();
            let count = || painted_errors_json()["count"].as_u64().unwrap();
            let base = count();

            label.set_visible(true);
            observe_painted_errors();
            assert_eq!(count(), base + 1, "{}", painted_errors_json());
            observe_painted_errors();
            assert_eq!(count(), base + 1, "a standing error counts once");

            label.set_visible(false);
            observe_painted_errors();
            label.set_visible(true);
            observe_painted_errors();
            assert_eq!(count(), base + 2, "raised again counts again");
            window.destroy();
        });
    }
}
