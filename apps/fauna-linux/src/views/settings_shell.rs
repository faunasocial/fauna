use adw::prelude::*;
use std::rc::Rc;
use std::sync::Arc;

use crate::client::FaunaClient;
use crate::i18n::strings::settings::member_review_page as member_review;
use crate::i18n::strings::{
    atproto_settings, common, devices, folders, labeler_catalog, linked_nests, logs, mail_aliases,
    mail_export, mail_import, mail_lists, mail_settings, mail_spam, muted_words, nostr,
    personalization, settings, status, subscriptions, web_settings,
};
use crate::views::status::StatusHandles;

/// Handles the settings shell hands back to `app.rs`.
pub struct SettingsShellHandles {
    /// `settings-nav-back` — the rail-top "leave settings" row. `app.rs` wires
    /// the click (it owns the content stack + sidebar needed to switch back to
    /// the non-settings app), mirroring `admin-nav-back`.
    pub nav_back: gtk::Button,
    /// The sub-stack the rail switches. `app.rs` seats it on [`CANONICAL_ENTRY`]
    /// whenever the content stack *enters* this shell — the canonical-entry rule
    /// (`docs/goal/ui/README.md` § Navigation model): sub-page position is shell
    /// state, not session state, so it never survives leaving the shell.
    pub sub_stack: gtk::Stack,
    /// The Status sub-page's live-update handles. The Status sub-page is the only
    /// settings sub-page that displays live nest data (identity, quota, node), so
    /// these are threaded into `app.rs`'s `widgets` struct exactly as the former
    /// standalone status view's handles were — the QuotaLoaded / NodeInfo /
    /// identity message handlers keep updating them unchanged.
    pub status: StatusHandles,
    /// The Devices + Folders sub-pages' handles (shared `DevicesMachine` +
    /// folder list box). Threaded into `app.rs`'s `widgets` so the
    /// `FolderMembersLoaded` dispatch + the on-auth / page-visible refreshes
    /// reach the machine, exactly as the former top-level Peers page did.
    pub devices_folders: crate::views::devices_folders::DevicesFoldersHandles,
    /// The Personalization + Community-labelers sub-pages' handles (shared
    /// `LabelerCatalogMachine`). Threaded into `app.rs`'s `widgets` so the
    /// on-auth / page-visible refreshes reach the machine.
    pub personalization: crate::views::personalization::PersonalizationHandles,
    /// Re-drive one store-backed sub-page's own load — what `app.rs`'s
    /// `AccountStoreChanged` handler calls for the OPEN one
    /// (`crate::store_surfaces`).
    pub store_resync: Rc<dyn Fn(crate::store_surfaces::StoreSurface)>,
}

/// The sub-page this shell is entered on — the first rail entry, Status.
/// `app.rs` seats the sub-stack here on every nav edge into the shell, per the
/// canonical-entry rule (`docs/goal/ui/README.md` § Navigation model). Named
/// rather than inlined so the rule's two implementation sites (here and admin's
/// twin) are greppable from the doc.
pub const CANONICAL_ENTRY: &str = "status";

/// Build the **Settings shell** as a vertical sidebar-swap (settings.md
/// § Navigation model — the desktop-client shape, mirroring `admin.md`
/// § Navigation model). Returns `(content, settings_sidebar, handles)`:
///
/// - `content` is the settings sub-stack (the page area, the main content-stack's
///   "settings" child). The inner `gtk::Stack` stays a DIRECT child of `content`
///   so the test agent's settings sub-page walker (`main.rs`) finds it.
/// - `settings_sidebar` is the vertical rail (`settings-nav-back` on top + the
///   shared `build_nav_rail` icon list) that `app.rs` swaps into the split-view
///   sidebar slot while the settings view is showing.
///
/// Replaces BOTH the former `adw::PreferencesWindow` modal (the cogwheel) AND the
/// status-page settings-embed hack (the scroll-of-short-sections that forced the
/// window wide). The first rail entry is **Status** — the former standalone
/// status page, folded in (user decision, 2026-06-03). All live-data display
/// lives there (it carries the live `StatusHandles`); the other sub-pages are
/// built once and are static/self-fetching, exactly as in the old modal.
pub fn build_settings_shell(
    client: &Rc<FaunaClient>,
    on_navigate_to_feed: Rc<dyn Fn()>,
) -> (gtk::Box, gtk::Box, SettingsShellHandles) {
    // Most sub-pages self-wire from the registered settings client; the Devices +
    // Folders sub-pages need the live `FaunaClient` (their shared `DevicesMachine`
    // rides its WS-RPC connection, and the folder rows lazy-load member rosters).

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let stack = gtk::Stack::new();
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);

    // `settings-nav-back` — the uniform "leave settings" affordance, at the top
    // of the rail so it is present on every settings sub-page (mirrors
    // `admin-nav-back`). The exit target is wired in `app.rs`. Rendered icon +
    // label so it reads as a row in the vertical rail.
    let nav_back_content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    nav_back_content.append(&gtk::Image::from_icon_name("go-previous-symbolic"));
    nav_back_content.append(&gtk::Label::new(Some(settings::EXIT_SETTINGS)));
    let nav_back = gtk::Button::builder().child(&nav_back_content).build();
    nav_back.set_widget_name("settings-nav-back");
    nav_back.set_tooltip_text(Some(settings::EXIT_SETTINGS));
    nav_back.add_css_class("flat");
    nav_back.set_halign(gtk::Align::Fill);
    nav_back.set_margin_top(8);
    nav_back.set_margin_start(8);
    nav_back.set_margin_end(8);
    nav_back.set_margin_bottom(8);

    // --- Status sub-page (FIRST → the shell's default child). The former
    // standalone status view, folded in: identity, connection, sync, p2p, quota,
    // node info. Owns the live `StatusHandles`. ---
    let (status_page, status_handles) = crate::views::status::build_status_view();
    stack.add_titled(&status_page, Some("status"), common::STATUS);

    // --- The remaining sub-pages = the former modal's `adw::PreferencesPage`s,
    // one per rail entry. Each self-wires from the registered settings client
    // (like the old modal tabs); built once, exactly as the modal built them on
    // each open (the static/self-fetching ones tolerate build-once). The child
    // name = the e2e sub-page id (`{"view":"settings","id":"<name>"}`). The
    // `add_titled` title argument is metadata only (the custom `build_nav_rail`
    // renders the rail labels, not a stack switcher) but is kept on the same
    // shared i18n consts as the rail for a single source of truth (priority #1/#3).
    // The Account page's Stage-2 require-confirm toggles cache the registry flag at
    // build time; `account_refresh` re-reads it on visible (the admin auto-default
    // writes the flag after this shell is built). Wired below.
    let (account_page, account_refresh) = crate::settings::account::build_account_page();
    stack.add_titled(&account_page, Some("account"), common::ACCOUNT);
    // Members To Review — the permanent post-succession unattested-member
    // review (`succession-aftermath.md` § Propagation ruling 1, item (iv)),
    // placed directly after Account per the rail slot ratified in
    // `settings.md` § Navigation model. `member_review_refresh` re-reads the
    // roster on this page's own nav edge (below): opened rarely, and exactly
    // where a verdict another device recorded must not be re-asked.
    let (member_review_page, member_review_refresh) =
        crate::settings::member_review::build_member_review_page(client);
    stack.add_titled(
        &member_review_page,
        Some("member-review"),
        member_review::TITLE,
    );
    // Subscriptions — the consumer "my subscriptions" page (the author-side tier
    // management lives on the profile Tiers tab). Observer-free, so it returns a
    // refresh closure we wire to the stack's visible-child notify below.
    let (subscriptions_page, subscriptions_refresh) =
        crate::settings::subscriptions::build_subscriptions_page();
    stack.add_titled(
        &subscriptions_page,
        Some("subscription-settings"),
        subscriptions::TITLE,
    );
    // Privacy's email-filter list is build-once like the rest, so it re-reads
    // on visible below: the succession aftermath raises the inherited-rule
    // marks after the successor's shell is already built.
    let (privacy_page, privacy_refresh) = crate::settings::privacy::build_privacy_page(client);
    stack.add_titled(&privacy_page, Some("privacy"), settings::PRIVACY);
    // Muted words — the tier-1 user keyword filter (moderation.md § Muted
    // keywords; content-moderation-and-ranking.md § Q3), placed right after
    // Privacy in the rail as its sibling personal content-filtering surface
    // (settings.md § Navigation model). `muted_words_refresh` re-reads on
    // visible below — a term added elsewhere (another device, or since W5.6 (account-data-plane.md § Workstreams) a
    // concurrent same-account instance) must not wait for a relaunch.
    let (muted_words_page, muted_words_refresh) =
        crate::settings::muted_words::build_muted_words_page(client);
    stack.add_titled(&muted_words_page, Some("muted-words"), muted_words::TITLE);
    // Personalization home + Community labelers catalog — the unified
    // authoring surface for the user's single tier-1 ruleset
    // (content-moderation-and-ranking.md § Composition + § Tier-3). Built
    // together (like Devices + Folders) so the shared `LabelerCatalogMachine`
    // + render loop are shared; the home links to Feeds (exits Settings) and
    // to Muted words / Community labelers (switches this same settings stack).
    let on_navigate_to_muted_words: Rc<dyn Fn()> = {
        let stack = stack.clone();
        Rc::new(move || stack.set_visible_child_name("muted-words"))
    };
    let on_navigate_to_catalog: Rc<dyn Fn()> = {
        let stack = stack.clone();
        Rc::new(move || stack.set_visible_child_name("labeler-catalog"))
    };
    let (personalization_page, labeler_catalog_page, personalization_handles) =
        crate::views::personalization::build_personalization_and_catalog_pages(
            client,
            on_navigate_to_feed,
            on_navigate_to_muted_words,
            on_navigate_to_catalog,
        );
    stack.add_titled(
        &personalization_page,
        Some("personalization"),
        personalization::TITLE,
    );
    stack.add_titled(
        &labeler_catalog_page,
        Some("labeler-catalog"),
        labeler_catalog::TITLE,
    );
    // The General page caches its build-time tray-host state; `general_refresh`
    // re-evaluates it on visible (a tray host can appear/disappear at runtime),
    // so the close-to-tray row greys/un-greys to match (Track 4).
    let (general_page, general_refresh) =
        crate::settings::general::build_general_page(client.runtime_handle());
    stack.add_titled(&general_page, Some("general"), settings::GENERAL);
    stack.add_titled(
        &crate::settings::encryption::build_encryption_page(),
        Some("encryption"),
        settings::encryption_page::TITLE,
    );
    // Devices (roster) + Folders (control plane) — two sub-pages over one shared
    // `DevicesMachine` (2026-06-28 unification; the former top-level "Peers" page +
    // "Sync" sub-page folded in here). Built together so the machine + render loop
    // are shared; their handles thread up to `app.rs` via `SettingsShellHandles`.
    let (devices_page, folders_page, devices_folders_handles) =
        crate::views::devices_folders::build_devices_and_folders_pages(client);
    stack.add_titled(&devices_page, Some("devices"), devices::TITLE);
    stack.add_titled(&folders_page, Some("folders"), folders::TITLE);
    stack.add_titled(
        &crate::settings::p2p_tab::build_p2p_page(),
        Some("p2p"),
        status::p2p::TITLE,
    );
    stack.add_titled(
        &crate::settings::nostr_tab::build_nostr_page(),
        Some("nostr"),
        nostr::TITLE,
    );
    // AT Protocol — the ATProto settings page (atproto-pds-full.md
    // § App surface), placed after Nostr as the sibling federation-protocol
    // bridge settings page (settings.md § Navigation model).
    stack.add_titled(
        &crate::settings::atproto::build_atproto_settings_page(),
        Some("atproto"),
        atproto_settings::TITLE,
    );
    stack.add_titled(
        &crate::settings::mail::build_mail_page(),
        Some("mail-settings"),
        mail_settings::TITLE,
    );
    let (mail_aliases_page, mail_aliases_refresh) =
        crate::settings::mail_aliases::build_mail_aliases_page();
    stack.add_titled(
        &mail_aliases_page,
        Some("mail-aliases"),
        mail_aliases::TITLE,
    );
    stack.add_titled(
        &crate::settings::mail_spam::build_mail_spam_page(),
        Some("mail-spam"),
        mail_spam::TITLE,
    );
    stack.add_titled(
        &crate::settings::mail_export::build_mail_export_page(),
        Some("mail-export"),
        mail_export::TITLE,
    );
    stack.add_titled(
        &crate::settings::mail_import::build_mail_import_page(),
        Some("mail-import"),
        mail_import::TITLE,
    );
    // Lists + Members share one shared-machine-per-row navigation, mirroring
    // Personalization's `on_navigate_to_muted_words` callback pattern: Members
    // is built first so its `select` closure exists, then composed with a
    // stack switch and threaded into Lists' row builder.
    let (mail_list_members_page, mail_list_members_select, mail_list_members_resolve_fallback) =
        crate::settings::mail_list_members::build_mail_list_members_page();
    let on_navigate_to_members: Rc<dyn Fn(String, String)> = {
        let stack = stack.clone();
        Rc::new(move |list_id_hex: String, friendly_name: String| {
            mail_list_members_select(list_id_hex, friendly_name);
            stack.set_visible_child_name("mail-list-members");
        })
    };
    let (mail_lists_page, mail_lists_refresh) =
        crate::settings::mail_lists::build_mail_lists_page(on_navigate_to_members);
    stack.add_titled(&mail_lists_page, Some("mail-lists"), mail_lists::TITLE);
    stack.add_titled(
        &mail_list_members_page,
        Some("mail-list-members"),
        mail_lists::MEMBERS_TITLE,
    );
    let (web_page, web_refresh) = crate::settings::web::build_web_page();
    stack.add_titled(&web_page, Some("web"), web_settings::TITLE);
    // Nests — re-hydrated on becoming visible (its trust facet's mint-option
    // catalog depends on state that changes after login: tier custody, holder
    // roster, grants minted elsewhere).
    let (linked_nests_page, linked_nests_refresh) =
        crate::settings::linked_nests::build_linked_nests_page();
    let linked_nests_refresh: Rc<dyn Fn()> = Rc::new(linked_nests_refresh);
    stack.add_titled(&linked_nests_page, Some("nests"), linked_nests::TITLE);
    // Task delegation — the cross-participant capstone of the participant cluster
    // (settings.md § Navigation model — placed after Nests). Renders the shared
    // `fauna-client-delegation` view-model (per-kind runner + assignment picker).
    let (task_delegation_page, task_delegation_refresh) =
        crate::settings::task_delegation::build_task_delegation_page();
    stack.add_titled(
        &task_delegation_page,
        Some("task-delegation"),
        crate::settings::task_delegation::TITLE,
    );
    // Connected apps — the one roster of everything acting for the user from
    // outside the seven apps (settings.md § Navigation model — directly after
    // Task delegation). Re-reads on every visit; the machine is built on the
    // first one, after the Mail & Calendar page has published its mail machine.
    let (connected_apps_page, connected_apps_refresh) =
        crate::settings::connected_apps::build_connected_apps_page();
    stack.add_titled(
        &connected_apps_page,
        Some("connected-apps"),
        crate::settings::connected_apps::TITLE,
    );
    // Logs — the client's durable in-app log record (observability.md § Surfaces).
    // Self-wires from the process-global `fauna_log` ring (no client handle).
    stack.add_titled(
        &crate::settings::logs::build_logs_page(),
        Some("logs"),
        logs::TITLE,
    );

    // The `subscription-settings` page is observer-free; re-read its
    // `mine.list` whenever it becomes the visible sub-page (the author may have
    // approved a pending request, or the user subscribed elsewhere). Mirrors the
    // profile Tiers-tab on-visible refresh.
    //
    // Also half of the nav-edge-AWAY-from-`account` hook: leaving the Account
    // sub-page is the one acknowledgment gesture that discharges a pending
    // stolen-ceremony persist-failure message (`settings.md` § Recovery kit
    // → *The persist-failure message survives the page*) — the user has had the whole visit to read or
    // copy it — and performs a supersession the ceremony held back. No new
    // `ui.yaml` element: this reuses the same visible-child notify every other
    // on-visible refresh below already wires to. The other half is `app.rs`'s
    // content-stack notify (leaving the shell from Account); the pair and its
    // edge live in `settings::stolen_hold`. A fresh shell starts a fresh visit.
    crate::settings::reset_account_visit();
    let knock_list_client = Rc::clone(client);
    let devices_machine = Arc::clone(&devices_folders_handles.devices_machine);
    let devices_runtime = client.runtime_handle();
    // The Devices/Folders pages' `DevicesMachine` re-read: their map hook covers
    // arriving from elsewhere, this covers re-selecting the page already
    // showing (which re-notifies but never re-maps) — without it a device
    // enrolled while the Devices page was open never reached the roster, nor
    // the folder wizard seeded from it.
    let devices_nav_refresh = Rc::clone(&devices_folders_handles.devices_nav_refresh);
    let devices_refresh: Rc<dyn Fn()> = Rc::new(move || {
        let machine = Arc::clone(&devices_machine);
        devices_runtime.spawn(async move { machine.refresh().await });
    });
    // The store-change notice's re-drives (`crate::store_surfaces`): each
    // store-backed sub-page's own load, exactly what re-selecting the page
    // already showing runs below — reload semantics, no new render path. The
    // match is exhaustive on purpose: a new store-backed surface must say here
    // how it is re-driven.
    let store_resync: Rc<dyn Fn(crate::store_surfaces::StoreSurface)> = {
        use crate::store_surfaces::StoreSurface;
        let muted_words_refresh = Rc::clone(&muted_words_refresh);
        let linked_nests_refresh = Rc::clone(&linked_nests_refresh);
        let devices_refresh = Rc::clone(&devices_refresh);
        let devices_nav_refresh = Rc::clone(&devices_nav_refresh);
        Rc::new(move |surface| match surface {
            StoreSurface::MutedWords => muted_words_refresh(),
            StoreSurface::TaskDelegation => task_delegation_refresh(),
            // Followed folders and foreign sets ride the machine's refresh.
            StoreSurface::Folders => devices_refresh(),
            StoreSurface::Devices => {
                devices_refresh();
                devices_nav_refresh();
            }
            StoreSurface::Nests => linked_nests_refresh(),
            // A top-level page, not a sub-page of this shell: `app.rs`
            // re-drives it.
            StoreSurface::Feed => {}
        })
    };
    stack.connect_visible_child_name_notify(move |s| {
        let name = s.visible_child_name();
        crate::settings::note_settings_sub_page(name.as_deref() == Some("account"));
        match name.as_deref() {
            // Folders' "Shared with you" section: the folder-share knocks and
            // the co-present ceremony's consent cards. The page's own map hook
            // covers arriving from elsewhere; this arm covers re-selecting the
            // Folders page that is already showing, which re-notifies here but
            // never re-maps (`nav_rail::set_visible_child_forced`). A switch
            // between two sub-pages fires both hooks — the reads are peeks.
            Some("folders") => {
                knock_list_client.fetch_knock_lists();
                devices_refresh();
            }
            Some("devices") => {
                devices_refresh();
                // The custody facet and the store reads — page-level, not
                // `DevicesMachine` state.
                devices_nav_refresh();
            }
            Some("subscription-settings") => subscriptions_refresh(),
            Some("general") => general_refresh(),
            Some("nests") => linked_nests_refresh(),
            // The Connected apps roster is nest state read on every visit.
            Some("connected-apps") => connected_apps_refresh(),
            Some("account") => account_refresh(),
            // The permanent review page's nav-edge read: opened rarely, and
            // exactly where a verdict another device recorded must not be
            // re-asked, so it reads fresh on every visit rather than caching
            // across the shell's lifetime.
            Some("member-review") => member_review_refresh(),
            // mail-lists is observer-free like the pages above: its domain picker
            // is derived from the caller's own alias/list rows and goes stale the
            // moment one is added elsewhere (or on a fresh actor with none yet at
            // the one hydrate wire_machine did at shell-build time).
            Some("mail-lists") => mail_lists_refresh(),
            // A direct rail visit with nothing selected falls back to the
            // caller's first owned list (mail-mass-mailing.md § Per-app render
            // status — the tui shape, windows/apple's independent-machine
            // variant of it); a row's Members click already resolved this via
            // `on_navigate_to_members` above, so `resolve_fallback` no-ops then.
            Some("mail-list-members") => mail_list_members_resolve_fallback(),
            // mail-aliases MUST re-hydrate here, and this one is a correctness fix
            // rather than the freshness habit above: `render` gates add, generate
            // AND import on `default_domain`, whose only writer is the shared
            // machine's `refresh` — reached from the list round trip those very
            // buttons perform. Hydrating once at build time therefore DEADLOCKED
            // the page for any client that enabled mail after login: every control
            // that could fetch the domain was disabled, with no way forward from
            // the UI (measured 2026-08-28 at 60 s). tui applies the same nav-edge
            // hydrate to this page for the same stated reason.
            Some("mail-aliases") => mail_aliases_refresh(),
            // The Published-posts list is observer-free too: a post published
            // from the feed ⋯-menu, or from another device, must show up on
            // re-visit (web-content-hosting.md § Published-post management).
            Some("web") => web_refresh(),
            // Muted words is observer-free too: a term added elsewhere — another
            // device, or a concurrent same-account instance since W5.6
            // (account-scoping.md § Concurrent instances) — must show up on
            // re-visit, not wait for the one `wire`-time load this build-once page
            // would otherwise be stuck with.
            Some("muted-words") => muted_words_refresh(),
            // The filter list and its inherited-rule marks: a mark the
            // succession aftermath raised after this shell was built must show
            // on the next visit (succession-aftermath.md § Adjudicating what
            // the aftermath carries across).
            Some("privacy") => privacy_refresh(),
            _ => {}
        }
    });

    content.append(&stack);
    stack.set_vexpand(true);

    // Left-align every page's content column (libadwaita centers it by default,
    // which leaves wide empty gutters on a maximized window).
    crate::views::layout::left_align_clamped_pages(&content);

    // The rail — `app.rs` swaps this into the split-view sidebar slot while the
    // settings view shows. Flat, one entry per sub-page (user decision: like
    // admin's flat rail, NOT a scroll of short sections). (child name, label,
    // symbolic icon) in rail order; names match the `add_titled` calls above.
    // (child name, label, icon, indent). The Mail sub-pages are indented under
    // the "Mail & Calendar" entry so they read as a nested group ("Mail & Calendar"
    // → Aliases / Spam / Export mailbox / Lists / Members).
    // Labels come from the shared generated i18n consts (priority #1/#3/#4 —
    // "all apps share the same strings"), the same style `views/admin.rs`
    // uses for the admin nav. The Mail sub-page consts already carry the
    // prefix-less group form (Aliases / Spam / Export mailbox / Lists / Members).
    let nav: [(&'static str, &'static str, &'static str, bool); 27] = [
        ("status", common::STATUS, "emblem-system-symbolic", false),
        ("account", common::ACCOUNT, "avatar-default-symbolic", false),
        (
            "member-review",
            member_review::TITLE,
            "view-list-symbolic",
            false,
        ),
        (
            "subscription-settings",
            subscriptions::TITLE,
            "starred-symbolic",
            false,
        ),
        (
            "privacy",
            settings::PRIVACY,
            "channel-secure-symbolic",
            false,
        ),
        (
            "muted-words",
            muted_words::TITLE,
            "action-unavailable-symbolic",
            false,
        ),
        (
            "personalization",
            personalization::TITLE,
            "starred-symbolic",
            false,
        ),
        (
            "labeler-catalog",
            labeler_catalog::TITLE,
            "system-search-symbolic",
            true,
        ),
        (
            "general",
            settings::GENERAL,
            "preferences-other-symbolic",
            false,
        ),
        (
            "encryption",
            settings::encryption_page::TITLE,
            "security-high-symbolic",
            false,
        ),
        ("devices", devices::TITLE, "computer-symbolic", false),
        ("folders", folders::TITLE, "folder-symbolic", false),
        (
            "p2p",
            status::p2p::TITLE,
            "network-wireless-symbolic",
            false,
        ),
        (
            "nostr",
            nostr::TITLE,
            "network-transmit-receive-symbolic",
            false,
        ),
        (
            "atproto",
            atproto_settings::TITLE,
            "network-transmit-receive-symbolic",
            false,
        ),
        (
            "mail-settings",
            mail_settings::TITLE,
            "mail-unread-symbolic",
            false,
        ),
        (
            "mail-aliases",
            mail_aliases::TITLE,
            "mail-forward-symbolic",
            true,
        ),
        (
            "mail-spam",
            mail_spam::TITLE,
            "dialog-warning-symbolic",
            true,
        ),
        (
            "mail-export",
            mail_export::TITLE,
            "document-save-symbolic",
            true,
        ),
        (
            "mail-import",
            mail_import::TITLE,
            "mail-send-receive-symbolic",
            true,
        ),
        ("mail-lists", mail_lists::TITLE, "view-list-symbolic", true),
        (
            "mail-list-members",
            mail_lists::MEMBERS_TITLE,
            "system-users-symbolic",
            true,
        ),
        ("web", web_settings::TITLE, "network-server-symbolic", false),
        (
            "nests",
            linked_nests::TITLE,
            "network-server-symbolic",
            false,
        ),
        (
            "task-delegation",
            crate::settings::task_delegation::TITLE,
            "preferences-system-time-symbolic",
            false,
        ),
        (
            "connected-apps",
            crate::settings::connected_apps::TITLE,
            "network-wired-symbolic",
            false,
        ),
        (
            "logs",
            logs::TITLE,
            "utilities-system-monitor-symbolic",
            false,
        ),
    ];
    let settings_sidebar = crate::views::nav_rail::build_nav_rail(&nav_back, &nav, &stack);

    let handles = SettingsShellHandles {
        nav_back,
        sub_stack: stack.clone(),
        status: status_handles,
        devices_folders: devices_folders_handles,
        personalization: personalization_handles,
        store_resync,
    };

    (content, settings_sidebar, handles)
}
