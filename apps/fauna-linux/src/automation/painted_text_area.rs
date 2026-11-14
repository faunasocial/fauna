//! linux's leg of `fauna_e2e_agent::PAINTED_TEXT_AREA_KEY` — convention 17's
//! `painted-text-has-area` field (`tests/e2e-unified/helpers/frame_invariants.py::
//! _check_painted_text_has_area` owns the contract and the verdict; the shared
//! [`fauna_e2e_agent::PaintedTextArea`] keeps the starved entries and caps them).
//! This file only MEASURES: every showing widget with a test id and text of its
//! own, and the room GTK's layout gave it.
//!
//! **The room is the allocation capped by every clipping ancestor, never the
//! allocation alone.** GTK4 never allocates a widget below its measured
//! minimum: a label whose `gtk::Paned` side is shrunk to nothing keeps its own
//! text width as its allocation (measured, and pinned by
//! `painted_text_area_tests`) — WinUI's trap
//! (`apps/fauna-windows/FaunaApp/FaunaApp/Testing/PaintedTextAreaObserver.cs`)
//! in GTK's form. Where a container cannot give a child its minimum it either
//! hides the child (that paned side goes child-invisible, so nothing is
//! painted and nothing is published) or clips it. So a widget's room is its
//! bounds in the root's space cut by every ancestor with `overflow == Hidden`.
//!
//! **A scroll viewport clips by its size, never by its scroll position.** Its
//! content is laid out at full extent and a widget scrolled out of view keeps
//! its box, so entering a `gtk::ScrolledWindow` or a `gtk::Scrollable` (a
//! `gtk::Viewport`, a list) drops the clip — content coordinates are virtual
//! from there down — unless the viewport itself was given no area: GTK's
//! classic starve is a scroller in a box that propagates no minimum and is not
//! told to expand, which lays out zero rows tall, and everything inside it is
//! then laid out at full size and shown to nobody.
//!
//! **A revealer mid-animation is skipped** — its clip passes through every
//! height between zero and full for 250 ms by design, which is not a layout a
//! frame settles on.
//!
//! **Read once per PAINTED frame, never at state build.** A state read lands
//! between frames, where a page just built or switched to has not been laid
//! out yet: every widget on it still reads 0×0. Measured — the first linux
//! corpus walked at state build and read `125 frames observed, 35 violated`,
//! every page's heading and buttons `0×0` in panes like `10×10`, all on pages
//! a human sees whole. So the walk runs on each toplevel's frame clock
//! `after-paint` (layout done, the frame just shown), and the state builder
//! only reads the record ([`painted_text_area_json`]), first hooking any
//! toplevel not yet hooked ([`hook_search_roots`]) and asking it for a frame.
//! The shared record keeps the last painted frame, which is the one a human
//! sees.
//! A widget added after a frame's layout pass is mapped and painted-over with
//! no allocation yet, so `after-paint` still meets it at 0×0 — measured once
//! in the second linux corpus (`admin-dashboard-heading` on the admin shell's
//! first frame, clean in the run before). GTK never allocates text below its
//! minimum, so an EMPTY OWN allocation is never a starve, only a widget not
//! laid out yet: it is skipped, and measured at the next frame. A starve is
//! always a clip, which the room carries.
use super::{find, registry};
use fauna_e2e_agent::{LaidOutText, PaintedTextArea};
use gtk::graphene::Rect;
use gtk::prelude::*;
use serde_json::Value;
use std::cell::RefCell;

thread_local! {
    static PAINTED_TEXT_AREA: RefCell<PaintedTextArea> = RefCell::new(PaintedTextArea::default());
    static HOOKED_CLOCKS: RefCell<Vec<gtk::glib::WeakRef<gtk::gdk::FrameClock>>> =
        const { RefCell::new(Vec::new()) };
}

/// Hook every search root's frame clock not hooked yet, so each painted frame
/// is observed, and ask a newly hooked root for a frame. Idempotent per clock.
pub fn hook_search_roots() {
    for root in find::search_roots() {
        let Some(clock) = root.frame_clock() else {
            continue;
        };
        let known = HOOKED_CLOCKS.with_borrow_mut(|clocks| {
            clocks.retain(|c| c.upgrade().is_some());
            if clocks.iter().any(|c| c.upgrade().as_ref() == Some(&clock)) {
                return true;
            }
            clocks.push(clock.downgrade());
            false
        });
        if !known {
            clock.connect_after_paint(|_| observe_painted_text_area());
            root.queue_draw();
        }
    }
}

/// Observe the frame every search root just painted.
pub fn observe_painted_text_area() {
    let mut frame = Vec::new();
    for root in find::search_roots() {
        laid_out_text(&root, &mut frame);
    }
    PAINTED_TEXT_AREA.with_borrow_mut(|area| area.observe(frame));
}

/// The `painted_text_area` state value.
pub fn painted_text_area_json() -> Value {
    PAINTED_TEXT_AREA.with_borrow(PaintedTextArea::json)
}

/// Every showing, id-bearing text widget under `root` (inclusive), with its
/// room, in tree order.
pub(crate) fn laid_out_text(root: &gtk::Widget, out: &mut Vec<LaidOutText>) {
    visit(root, root, None, None, 0, out);
}

/// The text a widget paints of its OWN — never a descendant's, which carries
/// its own id or none. An explicit `set_test_text` declaration counts: it is
/// what the row presents as its text.
fn own_text(w: &gtk::Widget) -> Option<String> {
    if let Some(text) = crate::testid::test_text(w) {
        return Some(text);
    }
    if let Some(l) = w.downcast_ref::<gtk::Label>() {
        return Some(l.text().to_string());
    }
    if let Some(e) = w.downcast_ref::<gtk::Entry>() {
        return Some(e.text().to_string());
    }
    if let Some(b) = w.downcast_ref::<gtk::Button>() {
        return b.label().map(|s| s.to_string());
    }
    if let Some(c) = w.downcast_ref::<gtk::CheckButton>() {
        return c.label().map(|s| s.to_string());
    }
    None
}

/// `w`'s room: its bounds in `root`'s space cut by `clip` (`None`: no clip).
fn room(w: &gtk::Widget, root: &gtk::Widget, clip: Option<&Rect>) -> Rect {
    let empty = Rect::new(0.0, 0.0, 0.0, 0.0);
    let Some(bounds) = w.compute_bounds(root) else {
        return empty;
    };
    match clip {
        None => bounds,
        Some(clip) => bounds.intersection(clip).unwrap_or(empty),
    }
}

fn has_area(r: &Rect) -> bool {
    f64::from(r.width()) >= fauna_e2e_agent::MIN_READABLE_LAYOUT_UNIT
        && f64::from(r.height()) >= fauna_e2e_agent::MIN_READABLE_LAYOUT_UNIT
}

fn is_scroll(w: &gtk::Widget) -> bool {
    w.is::<gtk::ScrolledWindow>() || w.is::<gtk::Scrollable>()
}

fn visit(
    w: &gtk::Widget,
    root: &gtk::Widget,
    clip: Option<Rect>,
    pane: Option<(f64, f64)>,
    depth: u32,
    out: &mut Vec<LaidOutText>,
) {
    if let Some(r) = w.downcast_ref::<gtk::Revealer>()
        && r.reveals_child() != r.is_child_revealed()
    {
        return;
    }
    let own = room(w, root, clip.as_ref());
    let name = w.widget_name();
    // A widget with no allocation of its own was added after this frame's
    // layout pass (GTK never allocates text below its minimum, so an empty
    // allocation is never a starve): it is measured at the next frame.
    let allocated = w.width() > 0 && w.height() > 0;
    if allocated
        && registry::is_test_id(&name)
        && let Some(text) = own_text(w)
        && !text.trim().is_empty()
    {
        out.push(LaidOutText {
            id: name.to_string(),
            text,
            width: f64::from(own.width()),
            height: f64::from(own.height()),
            pane,
            depth,
        });
    }
    let child_clip = if is_scroll(w) {
        (!has_area(&own)).then_some(own)
    } else if w.overflow() == gtk::Overflow::Hidden {
        Some(own)
    } else {
        clip
    };
    let (child_pane, child_depth) = if has_area(&own) {
        (Some((f64::from(own.width()), f64::from(own.height()))), 0)
    } else {
        (pane, depth + 1)
    };
    let mut child = w.first_child();
    while let Some(c) = child {
        if find::is_showing(&c) {
            visit(&c, root, child_clip, child_pane, child_depth, out);
        }
        child = c.next_sibling();
    }
}

#[cfg(test)]
mod painted_text_area_tests {
    use super::*;
    use crate::testid::run_on_gtk_thread;
    use fauna_ui_ids as ids;

    /// Lay `window` out at its default size and return its laid-out text.
    fn lay_out(window: &gtk::Window) -> Vec<LaidOutText> {
        window.present();
        let ctx = gtk::glib::MainContext::default();
        // Pump until the first frame has allocated the tree.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while window.width() == 0 && std::time::Instant::now() < deadline {
            ctx.iteration(true);
        }
        while ctx.pending() {
            ctx.iteration(false);
        }
        let mut out = Vec::new();
        laid_out_text(window.upcast_ref(), &mut out);
        out
    }

    fn entry<'a>(frame: &'a [LaidOutText], id: &str) -> &'a LaidOutText {
        frame
            .iter()
            .find(|e| e.id == id)
            .unwrap_or_else(|| panic!("{id} not laid out: {frame:?}"))
    }

    /// GTK allocates a label on a pane shrunk to nothing its own minimum, and
    /// hides it: an unpainted label is never published, starved or not.
    #[test]
    fn a_shrunk_pane_hides_its_label_and_keeps_its_minimum() {
        run_on_gtk_thread(|| {
            let window = gtk::Window::new();
            window.set_default_size(400, 200);
            let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
            paned.set_shrink_start_child(true);
            paned.set_resize_start_child(false);
            let hidden = gtk::Label::new(Some("A heading the pane hides"));
            crate::testid::set_test_id(&hidden, ids::POST_SUBMIT_BUTTON);
            let fits = gtk::Label::new(Some("Fits"));
            crate::testid::set_test_id(&fits, ids::SMOKE_TARGET);
            paned.set_start_child(Some(&hidden));
            paned.set_end_child(Some(&fits));
            paned.set_position(0);
            window.set_child(Some(&paned));

            let frame = lay_out(&window);
            assert!(
                hidden.width() >= 1,
                "GTK allocated the label below its minimum ({}): the clip \
                 premise this measure rests on no longer holds",
                hidden.width()
            );
            assert!(!hidden.is_child_visible());
            assert!(
                frame.iter().all(|e| e.id != ids::POST_SUBMIT_BUTTON),
                "{frame:?}"
            );
            let f = entry(&frame, ids::SMOKE_TARGET);
            assert!(f.width >= 1.0 && f.height >= 1.0, "{f:?}");
            window.destroy();
        });
    }

    /// A scroller given no height starves its content: the label inside keeps
    /// its full allocation, its room is 0 rows, its pane is the box the
    /// scroller sits in, one box-less ancestor away — and the shared record
    /// keeps exactly it. An id-less label is never published.
    #[test]
    fn a_label_in_a_scroller_given_no_height_reads_starved() {
        run_on_gtk_thread(|| {
            let window = gtk::Window::new();
            window.set_default_size(300, 200);
            let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let starved = gtk::Label::new(Some("A heading nobody sees"));
            crate::testid::set_test_id(&starved, ids::PAGE_HEADING);
            let scroller = gtk::ScrolledWindow::new();
            scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::External);
            scroller.set_child(Some(&starved));
            page.append(&scroller);
            page.append(&gtk::Label::new(Some("no id")));
            window.set_child(Some(&page));

            let frame = lay_out(&window);
            assert!(starved.height() >= 1, "allocation {}", starved.height());
            let s = entry(&frame, ids::PAGE_HEADING);
            assert!(s.height < 1.0, "{s:?}");
            let pane = s.pane.expect("the page has a box");
            assert!(pane.0 >= 1.0 && pane.1 >= 1.0, "{s:?}");
            assert!(s.depth >= 1, "{s:?}");
            assert!(frame.iter().all(|e| e.text != "no id"), "{frame:?}");

            let mut area = PaintedTextArea::default();
            area.observe(frame);
            let json = area.json();
            let starved_ids: Vec<_> = json["starved"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["id"].as_str().unwrap().to_owned())
                .collect();
            assert_eq!(starved_ids, [ids::PAGE_HEADING], "{json}");
            window.destroy();
        });
    }

    /// A label added to a mapped page after its layout pass is mapped with no
    /// allocation yet: it is not laid out, so it is not published — never a
    /// 0×0 starve.
    #[test]
    fn a_label_not_laid_out_yet_is_not_published() {
        run_on_gtk_thread(|| {
            let window = gtk::Window::new();
            window.set_default_size(300, 200);
            let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
            window.set_child(Some(&page));
            lay_out(&window);
            let late = gtk::Label::new(Some("Added late"));
            crate::testid::set_test_id(&late, ids::PAGE_HEADING);
            page.append(&late);
            assert!(late.is_mapped(), "the premise: mapped before laid out");
            assert_eq!(late.width(), 0, "the premise: no allocation yet");
            let mut frame = Vec::new();
            laid_out_text(window.upcast_ref(), &mut frame);
            assert!(frame.iter().all(|e| e.id != ids::PAGE_HEADING), "{frame:?}");
            let ctx = gtk::glib::MainContext::default();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while late.width() == 0 && std::time::Instant::now() < deadline {
                ctx.iteration(true);
            }
            let mut frame = Vec::new();
            laid_out_text(window.upcast_ref(), &mut frame);
            let e = entry(&frame, ids::PAGE_HEADING);
            assert!(e.width >= 1.0 && e.height >= 1.0, "{e:?}");
            window.destroy();
        });
    }

    /// A label scrolled out of view keeps its box: a scroll position is no
    /// clip.
    #[test]
    fn a_label_scrolled_out_of_view_keeps_its_box() {
        run_on_gtk_thread(|| {
            let window = gtk::Window::new();
            window.set_default_size(300, 100);
            let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
            for i in 0..60 {
                column.append(&gtk::Label::new(Some(&format!("filler {i}"))));
            }
            let far = gtk::Label::new(Some("Far below"));
            crate::testid::set_test_id(&far, ids::PAGE_HEADING);
            column.append(&far);
            let scroller = gtk::ScrolledWindow::new();
            scroller.set_child(Some(&column));
            scroller.set_vexpand(true);
            window.set_child(Some(&scroller));

            let frame = lay_out(&window);
            let e = entry(&frame, ids::PAGE_HEADING);
            assert!(e.width >= 1.0 && e.height >= 1.0, "{e:?}");
            window.destroy();
        });
    }
}
