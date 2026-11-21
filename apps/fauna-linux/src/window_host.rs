//! The one window the process shows across its interactive life
//! (`onboarding.md` § Wizard exit handling → *The wizard becomes the app in
//! place*): the wizard's window becomes the launch progress view's, which
//! becomes the signed-in app's, by swapping its content — never a close and a
//! reopen.
//!
//! Two per-window facts follow the content, so they live here, keyed by the
//! window object rather than by whichever builder made it:
//!
//! - its close handler — the wizard's and the launch screen's quit the app, the
//!   signed-in window's closes to the tray — replaced, never stacked, when the
//!   content moves on ([`set_close_handler`]);
//! - what its root shows — the name `fauna_e2e_agent::ROOT_SURFACE_KEY`'s
//!   journal records for it ([`set_root_surface`]). Written for every build,
//!   read only by the e2e automation (`automation::toplevel_windows`).
//!
//! Main-thread only, like every widget.

use adw::prelude::*;
use gtk::glib;
use std::cell::RefCell;

thread_local! {
    static CLOSE_HANDLERS: RefCell<Vec<(glib::WeakRef<gtk::Window>, glib::SignalHandlerId)>> =
        const { RefCell::new(Vec::new()) };
    static ROOT_SURFACES: RefCell<Vec<(glib::WeakRef<gtk::Window>, &'static str)>> =
        const { RefCell::new(Vec::new()) };
}

/// Move Windows's content on to `content` — the one way a kept window takes its
/// next surface (wizard → launch → app). The window is made sensitive again
/// here: a window kept across an account switch went through
/// `quiesce_for_teardown` with every other window, and the surface it now
/// shows belongs to the incoming account, live from its first frame — left
/// inert, the append finish's `launch_retry` showed Retry and the account
/// switcher that refused every press.
pub fn host_content(win: &adw::ApplicationWindow, content: &impl IsA<gtk::Widget>) {
    win.set_content(Some(content));
    win.set_sensitive(true);
}

/// Install `handler` as Windows's close-request handler, disconnecting the one
/// this function installed before, if any — so a window whose content moves
/// from the wizard to the app stops quitting the app on close.
pub fn set_close_handler<W: IsA<gtk::Window>>(
    win: &W,
    handler: impl Fn(&gtk::Window) -> glib::Propagation + 'static,
) {
    let win = win.upcast_ref::<gtk::Window>();
    let id = win.connect_close_request(handler);
    let previous = CLOSE_HANDLERS.with_borrow_mut(|handlers| {
        handlers.retain(|(w, _)| w.upgrade().is_some());
        let at = handlers
            .iter()
            .position(|(w, _)| w.upgrade().as_ref() == Some(win));
        let previous = at.map(|i| handlers.remove(i).1);
        handlers.push((win.downgrade(), id));
        previous
    });
    if let Some(previous) = previous {
        win.disconnect(previous);
    }
}

/// Name what Windows's root shows now: one of the `fauna_e2e_agent::ROOT_SURFACE_*`
/// names or a launch surface's ui.yaml page.
pub fn set_root_surface<W: IsA<gtk::Window>>(win: &W, name: &'static str) {
    let win = win.upcast_ref::<gtk::Window>();
    ROOT_SURFACES.with_borrow_mut(|names| {
        names.retain(|(w, _)| w.upgrade().is_some());
        match names
            .iter_mut()
            .find(|(w, _)| w.upgrade().as_ref() == Some(win))
        {
            Some(entry) => entry.1 = name,
            None => names.push((win.downgrade(), name)),
        }
    });
    // The e2e journal exists only in debug and `e2e-agent` builds (`lib.rs`
    // gates `mod automation` the same way); a release build records nothing.
    #[cfg(any(debug_assertions, feature = "e2e-agent"))]
    crate::automation::toplevel_windows::observe_root_surface();
}

/// What Windows's root shows, if it is one of the app's root windows.
pub fn root_surface_of(win: &gtk::Window) -> Option<&'static str> {
    ROOT_SURFACES.with_borrow(|names| {
        names
            .iter()
            .find(|(w, _)| w.upgrade().as_ref() == Some(win))
            .map(|(_, name)| *name)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The append finish's switch quiesces every window, the wizard's
    /// included, then runs the new account's launch in that kept window
    /// (`onboarding.md` § Wizard exit handling → *The finished wizard holds
    /// until the session is signed in*). The surface it hosts next must take a
    /// press: Retry and the `launch_retry` account switcher were painted
    /// insensitive under the inert window.
    #[test]
    fn a_quiesced_kept_window_hosts_its_next_surface_live() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let win = adw::ApplicationWindow::builder().build();
            win.set_content(Some(&gtk::Button::with_label("wizard")));
            win.set_sensitive(false); // `quiesce_for_teardown`

            let retry = gtk::Button::with_label("retry");
            host_content(&win, &retry);
            assert!(
                retry.is_sensitive(),
                "the hosted surface's controls take a press"
            );
            win.destroy();
        });
    }

    /// Every builder that moves a kept window on goes through [`host_content`];
    /// a bare `host.set_content(` would host the next surface under a window a
    /// teardown left inert.
    #[test]
    fn every_kept_window_takes_its_content_through_host_content() {
        for (file, src) in [
            ("views/launch.rs", include_str!("views/launch.rs")),
            ("app.rs", include_str!("app.rs")),
        ] {
            assert!(
                !src.contains("host.set_content("),
                "{file} sets a kept window's content directly"
            );
            assert!(
                src.contains("crate::window_host::host_content(&host,"),
                "{file} hosts its surface through `window_host::host_content`"
            );
        }
    }
}
