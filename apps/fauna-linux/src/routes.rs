//! The `fauna://` in-app route's intake (`apps/linux.md` § System integration
//! → *In-app routes*).
//!
//! The desktop entry registers `x-scheme-handler/fauna` with `Exec=… %u`, and
//! the application sets `HANDLES_OPEN`, so a route reaches the running app on
//! GApplication's `open` signal — forwarded to the primary instance by the
//! same D-Bus uniqueness that raises it on a plain launch. The agent banner's
//! tap rides that leg too (the sync agent re-mints the route into the app's
//! argv), and the e2e `open_route` command feeds the same door. The grammar is
//! the one shared `fauna_core::app_route` — this file never parses a URI
//! itself.
//!
//! **One door, [`offer`].** It holds a route while no signed-in main window is
//! up and applies it once [`signed_in`] registers one; applying the consent
//! route lands on Settings → Connected apps, whose visit runs the shared
//! machine's `open_handoff` (`settings::connected_apps::open_handoff`); the
//! two share routes — the GNOME Files Share entry's — land on Media's
//! detail + create surface (`views::media::open_share_route`) and on the
//! Folders page's member-share flow
//! (`views::devices_folders::folders::open_share_route`).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use fauna_core::app_route::AppRoute;

/// The work a route's caller waits on — the e2e `open_route` ack. Runs on the
/// GTK main thread once the route's page work has painted, or at once when the
/// route was held or dropped.
pub type Then = Box<dyn FnOnce()>;

/// The signed-in main window, and the flag its UI pump watches: a sign-out
/// shuts the pump down before the window goes (and the authenticated window's
/// widget tree does not finalize on `destroy()`, so its weak ref alone cannot
/// say it is gone).
struct MainWindow {
    window: glib::WeakRef<adw::ApplicationWindow>,
    pump_shutdown: Rc<Cell<bool>>,
}

thread_local! {
    static MAIN_WINDOW: RefCell<Option<MainWindow>> = const { RefCell::new(None) };
    static PENDING: RefCell<Option<AppRoute>> = const { RefCell::new(None) };
}

/// Parse a URI handed to the app through the shared grammar. An unparseable
/// one is logged and dropped — never an error surface, since a stranger's link
/// must not be able to paint one.
pub fn parse(uri: &str) -> Option<AppRoute> {
    let route = AppRoute::parse(uri);
    if route.is_none() {
        tracing::warn!(target: "fauna_linux::routes", "dropping an unparseable fauna:// route");
    }
    route
}

/// **The one door a `fauna://` route takes** — the `open` leg and the e2e
/// `open_route` command alike. Applied now when a signed-in main window is up,
/// else held for [`signed_in`]. `then` runs once the route's page work has
/// painted (or at once, when the route is held).
pub fn offer(route: AppRoute, then: Then) {
    match live_window() {
        Some(window) => apply(&window, route, then),
        None => {
            hold(route);
            then();
        }
    }
}

/// Register the signed-in main window and apply a held route on it. Called by
/// `launch_authenticated` once the window exists — every authenticated launch
/// (first login, returning-user boot, account switch) funnels through it. The
/// apply waits for an idle so the window's own first-paint navigation (the
/// restored sidebar item) has landed and cannot override the route's.
pub fn signed_in(window: &adw::ApplicationWindow, pump_shutdown: Rc<Cell<bool>>) {
    MAIN_WINDOW.with(|cell| {
        *cell.borrow_mut() = Some(MainWindow {
            window: window.downgrade(),
            pump_shutdown,
        })
    });
    glib::idle_add_local_once(|| {
        if let Some(window) = live_window()
            && let Some(route) = take_pending()
        {
            apply(&window, route, Box::new(|| {}));
        }
    });
}

fn live_window() -> Option<adw::ApplicationWindow> {
    MAIN_WINDOW.with(|cell| {
        let cell = cell.borrow();
        let main = cell.as_ref()?;
        if main.pump_shutdown.get() {
            return None;
        }
        main.window.upgrade()
    })
}

/// Hold a route for the next sign-in. A later route replaces an earlier one:
/// the user's last link is the one they mean.
fn hold(route: AppRoute) {
    PENDING.with(|cell| *cell.borrow_mut() = Some(route));
}

fn take_pending() -> Option<AppRoute> {
    PENDING.with(|cell| cell.borrow_mut().take())
}

fn apply(window: &adw::ApplicationWindow, route: AppRoute, then: Then) {
    match route {
        AppRoute::Consent { request_uri } => {
            let nav_window = window.clone();
            crate::settings::connected_apps::open_handoff(request_uri, then, move || {
                crate::app::navigate_to_settings_subpage(&nav_window, "connected-apps");
            });
            crate::show_window(window);
        }
        // The file manager's Share entry (`apps/linux.md` § The file manager's
        // Share entry (GNOME Files), link 4): navigation only — each arm opens
        // a surface the user still acts on, and a set or file this account
        // cannot see leaves the page with nothing open.
        AppRoute::ShareLink { folder_id, path } => {
            let nav_window = window.clone();
            crate::views::media::open_share_route(
                window.upcast_ref(),
                folder_id,
                path,
                then,
                move || crate::app::navigate_to_section(&nav_window, "media"),
            );
            crate::show_window(window);
        }
        AppRoute::FolderShare { folder_id } => {
            let nav_window = window.clone();
            crate::views::devices_folders::folders::open_share_route(folder_id, then, move || {
                crate::app::navigate_to_settings_subpage(&nav_window, "folders");
            });
            crate::show_window(window);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HANDLE: &str = "urn:ietf:params:oauth:request_uri:abc_DEF-123";

    fn consent() -> AppRoute {
        AppRoute::Consent {
            request_uri: HANDLE.into(),
        }
    }

    /// The `open` leg's carriage: a `fauna://` command-line argument becomes a
    /// GFile whose URI is the route verbatim — what the `open` handler parses.
    #[test]
    fn a_fauna_argument_reaches_the_open_leg_verbatim() {
        let uri = consent().to_uri();
        let file = gio::File::for_commandline_arg(&uri);
        assert_eq!(file.uri().as_str(), uri);
        assert_eq!(parse(&file.uri()), Some(consent()));
    }

    #[test]
    fn an_unparseable_uri_is_dropped() {
        assert_eq!(parse("fauna://nowhere"), None);
        assert_eq!(parse("https://example.com/"), None);
        let file = gio::File::for_commandline_arg("/tmp/not-a-route");
        assert_eq!(parse(&file.uri()), None);
    }

    /// Signed out, the door holds the route (and releases its caller at once);
    /// the held route is the one a sign-in takes, once, the latest winning.
    #[test]
    fn signed_out_the_route_is_held_once_and_the_last_one_wins() {
        let released = Rc::new(Cell::new(0));
        for request_uri in ["urn:ietf:params:oauth:request_uri:first", HANDLE] {
            let released = Rc::clone(&released);
            offer(
                AppRoute::Consent {
                    request_uri: request_uri.into(),
                },
                Box::new(move || released.set(released.get() + 1)),
            );
        }
        assert_eq!(released.get(), 2, "a held route must not stall its caller");
        assert_eq!(take_pending(), Some(consent()));
        assert_eq!(take_pending(), None, "a held route applies once");
    }
}
