//! Lazy paint of an over-budget text block's line runs (render-model.md § Implementation
//! status today → *Text-block line runs*).
//!
//! **Why.** One `gtk::Label` over a whole multi-megabyte paragraph freezes the app: Pango's
//! layout of a single string is super-linear in its hard line breaks (the UI-thread stall
//! stack is `g_utf8_strlen` under `pango_layout_get_size` — a per-line walk from the start
//! of the text), and a ~3 MiB, ~40 000-line plain-text mail measured ~76 s
//! (mail-message-size.md § Implementation status today owns the number). The split itself
//! is shared Rust (`fauna_core::render::inline_line_runs` / `text_line_runs`); this module
//! is only the GTK half — one label per run, and **only the runs near the viewport laid
//! out**, the peer of apple's `LazyVStack` and windows' `ItemsRepeater`.
//!
//! **How.** GTK has no virtualizing container that nests inside another scroller (a
//! `GtkListView` virtualizes against its *own* adjustment, so inside the thread's
//! `ScrolledWindow` it would realize every row). So each run gets its label up front —
//! an empty `gtk::Label` is cheap — reserved at an estimated height, and a label is given
//! its markup only once it comes within a page of the enclosing `ScrolledWindow`'s
//! viewport. The estimate is the measured height of the first run, which is painted
//! eagerly for that purpose; hard-broken mail lines rarely wrap, so it is near exact, and
//! where it is not the adjustment's `changed` re-runs the pass until it settles. A painted
//! run stays painted: the cost is paid once, as the user reaches it.
//!
//! A block with no `ScrolledWindow` above it when mapped paints every run at once — linear,
//! since each label stays within the budget — so the module never depends on its host.
//!
//! The consequence for automation: a run off screen has no text in its label, so a body
//! that contains one of these boxes declares its read from the document
//! (`document::render_to_widget`), exactly as windows' `DocumentBodyView` does.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use gtk::prelude::*;

/// Line height assumed when the first run cannot be measured (it always can once GTK is up;
/// this only keeps the reservation non-zero).
const FALLBACK_LINE_PX: i32 = 18;

/// One line run: its label, and its markup until the label has been given it.
struct Run {
    label: gtk::Label,
    pending: RefCell<Option<String>>,
}

impl Run {
    fn paint(&self) {
        if let Some(markup) = self.pending.take() {
            self.label.set_markup(&markup);
            self.label.set_size_request(-1, -1);
        }
    }
}

struct LineRuns {
    runs: Vec<Run>,
    /// A viewport pass is queued on the main loop — coalesces a burst of scroll events.
    pass_queued: Cell<bool>,
    /// The enclosing scroller's adjustment and our two handlers on it, while mapped.
    watching: RefCell<Option<(gtk::Adjustment, [glib::SignalHandlerId; 2])>>,
}

/// Build the box for a block split into `markups` (one Pango-markup string per shared line
/// run, in order; more than one). `classes` are the block's CSS classes — carried by the
/// box, so a code block's background and padding wrap the runs as one block instead of
/// repeating per run (font and colour inherit into the labels). `make_label` builds the
/// caller's ordinary block label from markup, so a run looks and behaves like any other.
pub fn build(
    markups: Vec<String>,
    classes: &[&str],
    make_label: impl Fn(&str) -> gtk::Label,
) -> gtk::Box {
    let runs_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    for c in classes {
        runs_box.add_css_class(c);
    }

    let mut line_px = FALLBACK_LINE_PX;
    let mut runs = Vec::with_capacity(markups.len());
    for (i, markup) in markups.into_iter().enumerate() {
        let lines = markup.matches('\n').count() as i32 + 1;
        let run = if i == 0 {
            // The measuring run: painted now, its natural height sets every reservation.
            let label = make_label(&markup);
            runs_box.append(&label);
            let (_, natural, _, _) = label.measure(gtk::Orientation::Vertical, -1);
            if natural > 0 {
                line_px = (natural / lines).max(1);
            }
            Run {
                label,
                pending: RefCell::new(None),
            }
        } else {
            let label = make_label("");
            label.set_size_request(-1, lines * line_px);
            runs_box.append(&label);
            Run {
                label,
                pending: RefCell::new(Some(markup)),
            }
        };
        runs.push(run);
    }

    let state = Rc::new(LineRuns {
        runs,
        pass_queued: Cell::new(false),
        watching: RefCell::new(None),
    });
    runs_box.connect_map({
        let state = state.clone();
        move |b| state.watch(b)
    });
    runs_box.connect_unmap(move |_| state.unwatch());
    runs_box
}

impl LineRuns {
    /// On map: follow the enclosing scroller, or — with none — paint everything.
    fn watch(self: &Rc<Self>, runs_box: &gtk::Box) {
        self.unwatch();
        let Some(scroller) = runs_box
            .ancestor(gtk::ScrolledWindow::static_type())
            .and_downcast::<gtk::ScrolledWindow>()
        else {
            self.runs.iter().for_each(Run::paint);
            return;
        };
        let adjustment = scroller.vadjustment();
        let on_change = || {
            let state = Rc::downgrade(self);
            let scroller = scroller.downgrade();
            move |_: &gtk::Adjustment| LineRuns::queue_pass(&state, &scroller)
        };
        let handlers = [
            adjustment.connect_value_changed(on_change()),
            adjustment.connect_changed(on_change()),
        ];
        *self.watching.borrow_mut() = Some((adjustment, handlers));
        LineRuns::queue_pass(&Rc::downgrade(self), &scroller.downgrade());
    }

    fn unwatch(&self) {
        if let Some((adjustment, handlers)) = self.watching.take() {
            for h in handlers {
                adjustment.disconnect(h);
            }
        }
    }

    /// Queue one viewport pass at idle priority — after the frame's layout, so the bounds it
    /// reads are current, and never from inside the size-allocate that emitted `changed`.
    fn queue_pass(state: &Weak<Self>, scroller: &glib::WeakRef<gtk::ScrolledWindow>) {
        let Some(this) = state.upgrade() else { return };
        if this.pass_queued.replace(true) {
            return;
        }
        let (state, scroller) = (state.clone(), scroller.clone());
        glib::idle_add_local_once(move || {
            let Some(this) = state.upgrade() else { return };
            this.pass_queued.set(false);
            if let Some(scroller) = scroller.upgrade() {
                this.paint_near_viewport(&scroller);
            }
        });
    }

    /// Paint every run within one page of the scroller's viewport. The runs are stacked in
    /// order, so the first candidate is found by bisection and the walk stops at the first
    /// run below the band — a scroll tick costs the runs it reveals, not the block's length.
    fn paint_near_viewport(&self, scroller: &gtk::ScrolledWindow) {
        let page = scroller.height() as f32;
        if page <= 0.0 {
            return;
        }
        let (top, bottom) = (-page, 2.0 * page);
        // A label that has never been allocated reports an empty rect at the origin — which
        // would read as "in view" for every run at once. Every run has a height once
        // allocated (its text, or its reservation), so zero means "no allocation yet".
        let bounds = |run: &Run| {
            (run.label.height() > 0)
                .then(|| run.label.compute_bounds(scroller))
                .flatten()
        };
        let first = self
            .runs
            .partition_point(|run| bounds(run).is_some_and(|b| b.y() + b.height() < top));
        for run in &self.runs[first..] {
            // Not allocated yet: the allocation's own `changed` brings the next pass.
            let Some(b) = bounds(run) else { break };
            if b.y() > bottom {
                break;
            }
            run.paint();
        }
    }
}
