//! The feed's paging gesture: the reader reaching the end of the post list
//! asks the manager for its next page — the scroll edge tui, web and android
//! page on (`docs/goal/ui/feed.md` § Errors & edge cases: the paginated fetch
//! is `has_more` + one in flight, with no load-more element). Shared by the
//! timeline and the profile's Posts tab, the two `FeedManager` windows.
//!
//! `FeedManager::load_more` is itself a no-op without `has_more` or while a
//! fetch is in flight; the latch here only keeps one arrival from spawning a
//! call per `value-changed` tick.

use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;

/// How near the bottom counts as reaching it — web's `onScroll` margin.
const EDGE_MARGIN_PX: f64 = 200.0;

/// Call `on_edge` once each time `scrolled`'s viewport arrives within
/// [`EDGE_MARGIN_PX`] of its bottom. The latch re-arms when the viewport
/// leaves that zone or the content grows (a page arrived), so a reader
/// resting at the edge of a list that just grew asks again on the next move.
pub fn wire(scrolled: &gtk::ScrolledWindow, on_edge: impl Fn() + 'static) {
    let adj = scrolled.vadjustment();
    let armed = Rc::new(Cell::new(true));
    {
        let armed = Rc::clone(&armed);
        adj.connect_value_changed(move |adj| {
            let near = adj.value() + adj.page_size() >= adj.upper() - EDGE_MARGIN_PX;
            if !near {
                armed.set(true);
            } else if adj.upper() > adj.page_size() && armed.replace(false) {
                on_edge();
            }
        });
    }
    adj.connect_upper_notify(move |_| armed.set(true));
}
