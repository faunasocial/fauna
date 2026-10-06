//! The top-level page set — ui.yaml `navigation.tabs`, one-to-one.
//!
//! Every app implements the same 11 canonical `{page}-tab` IDs; the gated
//! entries (`admin-tab`, `family-tab`) join once their gates (`am-i-admin`,
//! `fauna.family.status`) become checkable after login lands (M2).

use fauna_i18n::strings::{backups, common, conversations, events, family, media, nostr, profile};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Page {
    Feed,
    Conversations,
    Contacts,
    Profile,
    Events,
    Media,
    Backups,
    Nostr,
    Bridges,
    Notifications,
    /// The standalone Moderation page (`behavior/moderation.md`; ui.yaml
    /// `moderation`). Like [`Self::Search`] it is deliberately NOT in ui.yaml's
    /// `navigation.tabs` required-uniform set — but it is not gated either: the
    /// standalone apps all carry it as an always-visible row (linux's
    /// `SidebarItem::Moderation`, between Notifications and Settings; windows'
    /// standalone Moderation nav), and web embeds the same IDs in Settings
    /// (`moderation.md` § Architectural rules 1). tui takes linux's slot.
    Moderation,
    /// The global search page (`ui/search.md`; ui.yaml `search`). Unlike the
    /// other 11, `search-tab` is deliberately NOT in ui.yaml's
    /// `navigation.tabs` required-uniform set — each app picks its own
    /// placement (web/windows: a persistent nav entry; linux: Ctrl+F only, its
    /// own known e2e gap — `reachable: false` in `ui-actual-linux.yaml`). tui
    /// follows the richer, working pattern (web/windows), not linux's broken
    /// one (priority #4).
    Search,
    Settings,
    /// The nest-admin shell (`admin.md` § Navigation model; ui.yaml
    /// `navigation.gated_tabs` → `admin-tab`). **GATED**, so — unlike the 12
    /// above — it is deliberately NOT in [`Self::ALL`]: its `admin-tab` sidebar
    /// row joins the visible set only when the shared `fauna.account.am_i_admin`
    /// gate passes ([`crate::app::App::am_i_admin`]). It is the tui twin of
    /// linux's `SidebarItem::Admin` (appended after the always-visible rows,
    /// hidden until confirmed) and mirrors the [`Self::Settings`] shell exactly:
    /// a top-level page reached via its own tab, whose content is an inner
    /// sub-page set ([`crate::admin::AdminPage`], the "admin-style two-element
    /// nav" `automation::apply_nav` already documents).
    Admin,
    /// The family-safety surface (`family-safety.md` § App surface; ui.yaml
    /// `navigation.gated_tabs` → `family-tab`). **GATED** like [`Self::Admin`],
    /// so it too is deliberately outside [`Self::ALL`]: its `family-tab` sidebar
    /// row joins the visible set only when the post-auth `fauna.family.status`
    /// read reports a relationship — supervised, guarding, **or** named on an
    /// incoming transfer proposal ([`crate::app::App::has_family`], fail-closed
    /// until the read lands). Unlike Admin it is a single flat page, not a
    /// sub-page shell, so it needs no two-element nav.
    Family,
    /// Not a page — no content, never in [`Self::ALL`]. The bottom-of-sidebar
    /// quit affordance a live user asked for (2026-08-03): a terminal app has
    /// no window chrome to close, unlike the six GUI/mobile apps, so `q`/Ctrl+C
    /// (`App::handle_key`) is the only other way out and is undiscoverable.
    ///
    /// The sidebar ring *is* `App::page` (no second cursor state —
    /// `App::focus_next`'s doc comment), so moving onto this row DOES set
    /// `self.page = Page::Exit` like any other row — [`Self::label`] paints
    /// "Exit Fauna" highlighted over an empty body (`App::page_elements`'s
    /// `Page::Exit` arm), nothing more. Only ACTUATING it (Enter, click, or
    /// the e2e agent's `/element/click`) is special-cased, in the one shared
    /// dispatch door both actuation paths run through
    /// (`App::gesture_work`'s `Gesture::Nav(Page::Exit)` arm): instead of
    /// applying a nav, it sets `should_quit` directly.
    Exit,
}

impl Page {
    /// All standard pages. The first 10 plus `Settings` are ui.yaml
    /// `navigation.pages` display order (`navigation.tabs`, the required-uniform
    /// set); `Moderation` and `Search` are always-on additions beyond that
    /// required set (see their doc comments). `Moderation` sits where linux's
    /// sidebar puts it — immediately after Notifications.
    pub const ALL: [Page; 13] = [
        Page::Feed,
        Page::Conversations,
        Page::Contacts,
        Page::Profile,
        Page::Events,
        Page::Media,
        Page::Backups,
        Page::Nostr,
        Page::Bridges,
        Page::Notifications,
        Page::Moderation,
        Page::Search,
        Page::Settings,
    ];

    /// Sidebar label, from the shared generated string table — the same
    /// constants the linux sidebar uses.
    pub fn label(self) -> &'static str {
        match self {
            Page::Feed => common::FEED,
            Page::Conversations => conversations::list::TITLE,
            Page::Contacts => common::CONTACTS,
            Page::Profile => profile::TITLE,
            Page::Events => events::TITLE,
            Page::Media => media::TITLE,
            Page::Backups => backups::TITLE,
            Page::Nostr => nostr::TITLE,
            Page::Bridges => common::BRIDGES,
            Page::Notifications => common::NOTIFICATIONS,
            // The same shared label linux's Moderation row carries
            // (`views/sidebar.rs::label` → `common::MODERATION`).
            Page::Moderation => common::MODERATION,
            Page::Search => common::SEARCH,
            Page::Settings => common::SETTINGS,
            // The same shared label linux's gated Admin row carries
            // (`views/sidebar.rs::label` → `common::ADMIN`), priority #2/#3.
            Page::Admin => common::ADMIN,
            // …and the same for the gated Family row (`family::TITLE`).
            Page::Family => family::TITLE,
            Page::Exit => common::EXIT_FAUNA,
        }
    }

    /// The page's view name in the e2e state protocol
    /// (`set_state({"nav": {"stack": [{"view": "<name>"}]}})`).
    pub fn view_name(self) -> &'static str {
        match self {
            Page::Feed => "feed",
            Page::Conversations => "conversations",
            Page::Contacts => "contacts",
            Page::Profile => "profile",
            Page::Events => "events",
            Page::Media => "media",
            Page::Backups => "backups",
            Page::Nostr => "nostr",
            Page::Bridges => "bridges",
            Page::Notifications => "notifications",
            Page::Moderation => "moderation",
            Page::Search => "search",
            Page::Settings => "settings",
            Page::Admin => "admin",
            Page::Family => "family",
            Page::Exit => "exit",
        }
    }

    /// The ui.yaml `{page}-tab` element ID carried by this page's sidebar row.
    pub fn tab_id(self) -> &'static str {
        match self {
            Page::Feed => "feed-tab",
            Page::Conversations => "conversations-tab",
            Page::Contacts => "contacts-tab",
            Page::Profile => "profile-tab",
            Page::Events => "events-tab",
            Page::Media => "media-tab",
            Page::Backups => "backups-tab",
            Page::Nostr => "nostr-tab",
            Page::Bridges => "bridges-tab",
            Page::Notifications => "notifications-tab",
            Page::Moderation => "moderation-tab",
            Page::Search => "search-tab",
            Page::Settings => "settings-tab",
            Page::Admin => "admin-tab",
            Page::Family => "family-tab",
            Page::Exit => "exit-tab",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ui.yaml `navigation.tabs` is the lint-enforced REQUIRED-uniform set;
    /// this pins the sidebar to carry all of it, so drift shows up as a red
    /// test, not an e2e surprise. `Page::ALL` is a SUPERSET, not an exact
    /// match: `search-tab` is a real ui.yaml element deliberately left OUT of
    /// `navigation.tabs` (each app picks its own placement — `Page::Search`
    /// doc comment), and `gated_tabs` (admin-tab/family-tab) will be further
    /// additions once M8 wires their gates.
    #[test]
    fn tab_ids_include_every_ui_yaml_navigation_tab() {
        let ids: Vec<&str> = Page::ALL.iter().map(|p| p.tab_id()).collect();
        for required in [
            "feed-tab",
            "conversations-tab",
            "contacts-tab",
            "profile-tab",
            "events-tab",
            "media-tab",
            "backups-tab",
            "nostr-tab",
            "bridges-tab",
            "notifications-tab",
            "settings-tab",
        ] {
            assert!(
                ids.contains(&required),
                "{required} is a required ui.yaml navigation.tabs entry"
            );
        }
    }

    /// The tab ID is by convention `{view}-tab` — pin the two tables together.
    /// Includes the two gated pages (outside `Page::ALL`) explicitly, so the
    /// convention is proven for them too (`admin` → `admin-tab`, `family` →
    /// `family-tab`).
    #[test]
    fn tab_id_is_view_name_plus_tab() {
        for page in Page::ALL.iter().copied().chain([Page::Admin, Page::Family]) {
            assert_eq!(page.tab_id(), format!("{}-tab", page.view_name()));
        }
    }
}
