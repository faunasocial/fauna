//! The admin pages' reconnect re-read (`docs/goal/behavior/admin.md` § Errors
//! & edge cases, *Controls wait for their read*): when the nest connection comes
//! back, the admin page on screen asks its read again — with no navigation —
//! but only while that read has not answered, so a draft typed over an answered
//! page survives. The linux twin of tui's `admin::reconnect_reread_op` and web's
//! `admin-reread.ts`.
//!
//! Each admin page registers three answers when it is built: whether it is on
//! screen (GTK's map state — the admin sub-stack maps only its visible child,
//! and only while the shell is shown), whether its read has answered (the shared
//! machines' `read_state`, the NAT machine's `select_possibility`, or the page's
//! own read), and how to read again (the same read its entry fires).
//! [`on_reconnect`] runs from `app.rs`'s `WsEvent::Reconnected` arm. Every
//! entry to a page reads again through its own `connect_map`; this module adds
//! only the leg with no navigation.

use std::cell::RefCell;
use std::rc::Rc;

use fauna_core::possibility::ReadState;

type Answer = Rc<dyn Fn() -> bool>;

#[derive(Clone)]
struct Page {
    /// The admin sub-stack child name (`admin-calendar`, `settings`, …).
    name: &'static str,
    shown: Answer,
    answered: Answer,
    reread: Rc<dyn Fn()>,
}

thread_local! {
    static PAGES: RefCell<Vec<Page>> = const { RefCell::new(Vec::new()) };
}

/// Register one admin page. A page registered again under the same name
/// replaces the earlier entry (the page is built once per session; a rebuilt
/// shell must not leave a stale page behind).
pub fn register(
    name: &'static str,
    shown: impl Fn() -> bool + 'static,
    answered: impl Fn() -> bool + 'static,
    reread: impl Fn() + 'static,
) {
    PAGES.with(|pages| {
        let mut pages = pages.borrow_mut();
        pages.retain(|p| p.name != name);
        pages.push(Page {
            name,
            shown: Rc::new(shown),
            answered: Rc::new(answered),
            reread: Rc::new(reread),
        });
    });
}

/// [`register`] for a page whose on-screen answer is a widget's map state.
pub fn register_widget(
    name: &'static str,
    widget: &impl gtk::prelude::IsA<gtk::Widget>,
    answered: impl Fn() -> bool + 'static,
    reread: impl Fn() + 'static,
) {
    use gtk::prelude::{ObjectExt, WidgetExt};
    let widget = widget.as_ref().downgrade();
    register(
        name,
        move || widget.upgrade().is_some_and(|w| w.is_mapped()),
        answered,
        reread,
    );
}

/// Whether a read that keeps a [`ReadState`] has answered.
pub fn resolved(read: ReadState) -> bool {
    read == ReadState::Resolved
}

/// The nest connection came back: every admin page on screen whose read has
/// not answered reads again. Returns the pages that did, for the tests.
pub fn on_reconnect() -> Vec<&'static str> {
    // Clone the entries out first: a re-read may register or paint, and must
    // not run under the registry's borrow.
    let pages: Vec<Page> = PAGES.with(|pages| pages.borrow().clone());
    let mut reread = Vec::new();
    for page in pages {
        if (page.shown)() && !(page.answered)() {
            (page.reread)();
            reread.push(page.name);
        }
    }
    reread
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// `admin.md` § Errors & edge cases, *Controls wait for their read*: a
    /// failed read on the page on screen is asked again when the connection
    /// comes back; once it answered it is not (a typed draft survives); a page
    /// off screen is never read from here (its entry reads it).
    #[test]
    fn an_unanswered_admin_read_is_asked_again_on_reconnect() {
        let read = Rc::new(Cell::new(ReadState::Failed));
        let asked = Rc::new(Cell::new(0));
        let shown = Rc::new(Cell::new(true));
        {
            let read = Rc::clone(&read);
            let asked = Rc::clone(&asked);
            let shown = Rc::clone(&shown);
            register(
                "admin-calendar",
                move || shown.get(),
                move || resolved(read.get()),
                move || asked.set(asked.get() + 1),
            );
        }
        let off_screen_asked = Rc::new(Cell::new(0));
        {
            let asked = Rc::clone(&off_screen_asked);
            register(
                "settings",
                || false,
                || false,
                move || asked.set(asked.get() + 1),
            );
        }

        assert_eq!(on_reconnect(), vec!["admin-calendar"]);
        assert_eq!(asked.get(), 1, "a failed read is asked again");
        assert_eq!(off_screen_asked.get(), 0, "a page off screen is not read");

        // The re-read answered: the next reconnect leaves the page alone.
        read.set(read.get().after(true));
        assert!(on_reconnect().is_empty());
        assert_eq!(asked.get(), 1, "an answered page is not read again");

        // A failure after a success keeps the earlier answer — still answered.
        read.set(read.get().after(false));
        assert!(on_reconnect().is_empty());

        // Re-registering replaces: one entry per page.
        shown.set(false);
        register("admin-calendar", || true, || false, || {});
        assert_eq!(on_reconnect(), vec!["admin-calendar"]);
    }
}
