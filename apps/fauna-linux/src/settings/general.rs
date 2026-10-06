use adw::prelude::*;
use fauna_ui_ids as ids;
use std::path::PathBuf;
use std::sync::atomic::Ordering;

use crate::i18n::strings::common;
use crate::i18n::strings::settings;
use crate::i18n::strings::settings::general_page as gp;
use crate::tray;

/// Build the "General" preferences page, returning the page plus a refresh
/// closure the settings shell wires to its visible-child notify. The refresh
/// re-evaluates tray-host availability so the close-to-tray row greys/un-greys
/// when a system-tray host appears or disappears at runtime (the page itself is
/// built once and cached in the settings stack, so without this the row would
/// freeze at its build-time state — Track 4). `rt` is the client's runtime,
/// where the asked update check makes its round trip.
pub fn build_general_page(rt: tokio::runtime::Handle) -> (gtk::Box, impl Fn() + 'static) {
    let page = adw::PreferencesPage::builder()
        .title(settings::GENERAL)
        .icon_name("emblem-system-symbolic")
        .build();

    // --- Appearance group ---
    let appearance_group = adw::PreferencesGroup::builder()
        .title(settings::APPEARANCE)
        .build();

    let theme_row = adw::ComboRow::builder()
        .title(gp::THEME)
        .subtitle(gp::THEME_SUBTITLE)
        .build();

    let theme_items =
        gtk::StringList::new(&[gp::THEME_FOLLOW_SYSTEM, gp::THEME_LIGHT, gp::THEME_DARK]);
    theme_row.set_model(Some(&theme_items));

    // Load persisted preference and apply it.
    let saved = load_theme_preference();
    let initial_index = match saved {
        adw::ColorScheme::ForceLight => 1,
        adw::ColorScheme::ForceDark => 2,
        _ => 0, // Default / follow system
    };
    theme_row.set_selected(initial_index);

    theme_row.connect_selected_notify(move |row| {
        let scheme = match row.selected() {
            1 => adw::ColorScheme::ForceLight,
            2 => adw::ColorScheme::ForceDark,
            _ => adw::ColorScheme::Default,
        };
        adw::StyleManager::default().set_color_scheme(scheme);
        save_theme_preference(scheme);
    });

    appearance_group.add(&theme_row);
    page.add(&appearance_group);

    // --- Startup group ---
    let startup_group = adw::PreferencesGroup::builder()
        .title(settings::STARTUP)
        .build();

    let autostart_row = adw::SwitchRow::builder()
        .title(gp::LAUNCH_AT_LOGIN)
        .subtitle(gp::LAUNCH_AT_LOGIN_SUBTITLE)
        .build();
    crate::testid::set_test_id(&autostart_row, ids::SETTINGS_AUTOSTART_TOGGLE);

    // Show the *choice*, not the `.desktop` file's existence: file-presence
    // cannot distinguish "never chosen" from "deliberately turned off", which
    // is exactly why the choice is tri-state (`apps/linux.md` § Auto-start
    // at sign-in). A user who never chose sees the ON default, matching what
    // the post-auth hook registered on their behalf.
    //
    // The `set_active` below is programmatic, so it is signal-blocked — an
    // unblocked one would fire `connect_active_notify` on the default flip and
    // record a fake *explicit* choice on disk, defeating the default (the same
    // guard the close-to-tray row uses).
    let autostart_handler = autostart_row.connect_active_notify(move |row| {
        crate::autostart::apply_user_choice(row.is_active());
    });
    autostart_row.block_signal(&autostart_handler);
    autostart_row.set_active(crate::autostart::effective_choice());
    autostart_row.unblock_signal(&autostart_handler);

    startup_group.add(&autostart_row);
    page.add(&startup_group);

    // --- Behaviour group ---
    let behaviour_group = adw::PreferencesGroup::builder()
        .title(gp::BEHAVIOUR)
        .build();

    // When no system-tray host is present (stock GNOME with no
    // StatusNotifierWatcher), close-to-tray has nowhere to restore the window
    // from, so we grey the toggle with an explanatory note rather than let the
    // user enable a setting that would strand their window (Track 4). The host
    // can appear/disappear at runtime (e.g. enabling/disabling GNOME's
    // appindicator extension), so the `refresh` closure returned below re-runs
    // this evaluation whenever the page becomes visible again.
    let close_to_tray_row = adw::SwitchRow::builder()
        .title(settings::CLOSE_TO_TRAY_HEADER)
        .build();
    // Shared desktop platform_element test id (ui.yaml settings.platform_elements;
    // apps/windows.md + linux.md § System Tray) — one e2e drives linux + windows.
    crate::testid::set_test_id(&close_to_tray_row, ids::CLOSE_TO_TRAY_TOGGLE);

    // Only a genuine *user* toggle writes the saved preference. The refresh
    // closure below also touches `active` (to reflect host availability), so it
    // blocks this handler around its programmatic update to avoid clobbering the
    // saved CLOSE_TO_TRAY with a host-driven display change — which would also
    // record a fake *explicit* choice on disk and defeat the ON default.
    let saved_handler = close_to_tray_row.connect_active_notify(|row| {
        let value = row.is_active();
        tray::CLOSE_TO_TRAY.store(value, Ordering::SeqCst);
        if let Some(store) = crate::app_settings::AppSettingsStore::new() {
            store.set_close_to_tray(value);
        }
    });

    // Apply the current tray-host state, and return a closure that re-applies it.
    let apply_tray_host_state = {
        let row = close_to_tray_row.clone();
        move || {
            let host = tray::tray_host_available();
            row.set_subtitle(if host {
                settings::CLOSE_TO_TRAY_SUBTITLE
            } else {
                gp::NO_TRAY_SUBTITLE
            });
            row.set_sensitive(host);
            row.block_signal(&saved_handler);
            row.set_active(host && tray::CLOSE_TO_TRAY.load(Ordering::SeqCst));
            row.unblock_signal(&saved_handler);
        }
    };
    apply_tray_host_state();

    behaviour_group.add(&close_to_tray_row);

    let notification_sound_row = adw::SwitchRow::builder()
        .title(gp::NOTIFICATION_SOUND)
        .subtitle(gp::NOTIFICATION_SOUND_SUBTITLE)
        .active(tray::NOTIFICATION_SOUND.load(Ordering::SeqCst))
        .build();

    notification_sound_row.connect_active_notify(|row| {
        tray::NOTIFICATION_SOUND.store(row.is_active(), Ordering::SeqCst);
    });

    behaviour_group.add(&notification_sound_row);

    page.add(&behaviour_group);

    // --- Keyboard Shortcuts group ---
    let shortcuts_group = adw::PreferencesGroup::builder()
        .title(gp::KEYBOARD_SHORTCUTS)
        .description(gp::KEYBOARD_SHORTCUTS_DESCRIPTION)
        .build();

    let shortcut_rows: &[(&str, &str)] = &[
        ("Ctrl+N", gp::SHORTCUT_NEW_MESSAGE),
        ("Ctrl+Shift+N", gp::SHORTCUT_NEW_GROUP),
        ("Ctrl+E", gp::SHORTCUT_COMPOSE_EMAIL),
        ("Ctrl+W", gp::SHORTCUT_CLOSE_WINDOW),
        ("Ctrl+Shift+H", gp::SHORTCUT_HIDE_TO_TRAY),
        ("Escape", gp::SHORTCUT_MINIMIZE_TO_TRAY),
        ("Ctrl+Q", gp::SHORTCUT_QUIT),
        ("Ctrl+K", gp::SHORTCUT_QUICK_SWITCHER),
        ("Ctrl+F", common::SEARCH),
        ("Ctrl+,", gp::SHORTCUT_PREFERENCES),
        ("Ctrl+1..8", gp::SHORTCUT_SWITCH_SECTION),
    ];

    for (shortcut, description) in shortcut_rows {
        let row = adw::ActionRow::builder()
            .title(*description)
            .subtitle(*shortcut)
            .build();
        shortcuts_group.add(&row);
    }

    let shortcut_note = adw::ActionRow::builder()
        .title(gp::RAISE_WINDOW)
        .subtitle(gp::RAISE_WINDOW_SUBTITLE)
        .build();
    shortcuts_group.add(&shortcut_note);

    page.add(&shortcuts_group);

    // --- About group ---
    let about_group = adw::PreferencesGroup::builder()
        .title(settings::ABOUT)
        .build();
    // A caption/value row: `get_text` reads the subtitle — the one product
    // version every app renders (`product-version.md` § The model).
    let version_row = adw::ActionRow::builder()
        .title(gp::VERSION)
        .subtitle(env!("CARGO_PKG_VERSION"))
        .build();
    crate::testid::set_test_id(&version_row, ids::SETTINGS_APP_VERSION);
    about_group.add(&version_row);

    let update_button = gtk::Button::builder()
        .label(settings::CHECK_FOR_UPDATES)
        .halign(gtk::Align::Start)
        .build();
    crate::testid::set_test_id(&update_button, ids::SETTINGS_CHECK_UPDATES_BUTTON);
    // The round trip runs on the client's tokio runtime: the GTK main thread
    // holds no tokio context, so awaiting reqwest in a `spawn_local` panicked
    // the task and left the button on "Checking…" for good (`async_helper.rs`).
    update_button.connect_clicked(move |btn| {
        btn.set_sensitive(false);
        btn.set_label(common::CHECKING);
        let btn = btn.clone();
        crate::async_helper::spawn_with_snapshot(
            &rt,
            || async { crate::updater::check_for_update(&reqwest::Client::new()).await },
            move |answer| {
                use fauna_client::update_look::NewerReleaseCheck;
                match answer {
                    NewerReleaseCheck::Newer { tag } => {
                        btn.set_label(&gp::update_available(&tag));
                        show_update_notice(
                            tag.trim_start_matches('v'),
                            &fauna_core::version::release_page_url(&tag),
                        );
                        crate::updater::notify_update_available(&tag);
                    }
                    NewerReleaseCheck::UpToDate => {
                        btn.set_label(settings::UP_TO_DATE);
                    }
                    NewerReleaseCheck::Failed => {
                        btn.set_label(settings::CHECK_FAILED);
                    }
                }
                glib::timeout_add_seconds_local_once(3, move || {
                    btn.set_label(settings::CHECK_FOR_UPDATES);
                    btn.set_sensitive(true);
                });
            },
        );
    });
    about_group.add(&update_button);
    about_group.add(&update_notice_label());

    page.add(&about_group);

    (
        crate::testid::wrap_page_with_heading(settings::GENERAL, ids::PAGE_HEADING, &page),
        apply_tray_host_state,
    )
}

// ---------------------------------------------------------------------------
// The newer-version notice — `update-available-notice`
// ---------------------------------------------------------------------------

thread_local! {
    /// The notice's text once a check or the sign-in look found a newer release
    /// (session-local, never persisted), and the label painting it once the
    /// page is built. Both live on the GTK main thread: the sign-in look's
    /// `DataMessage::UpdateAvailable` can arrive before Settings was ever
    /// opened, so the text waits here and the label picks it up at build.
    static UPDATE_NOTICE: std::cell::RefCell<(Option<String>, Option<gtk::Label>)> =
        const { std::cell::RefCell::new((None, None)) };
}

/// The notice label for the About group: hidden — absent from the automation
/// tree — until a newer release is known.
fn update_notice_label() -> gtk::Label {
    let label = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .wrap(true)
        .selectable(true)
        .build();
    crate::testid::set_test_id(&label, ids::UPDATE_AVAILABLE_NOTICE);
    UPDATE_NOTICE.with_borrow_mut(|(text, slot)| {
        label.set_label(text.as_deref().unwrap_or_default());
        label.set_visible(text.is_some());
        *slot = Some(label.clone());
    });
    label
}

/// Paint the newer-version notice — the version and where to get it, the
/// shared `update_available_notice` words (`installers/README.md` § Knowing a
/// newer version is out). Called by the asked check and by the once-per-sign-in
/// look (`app.rs`'s `DataMessage::UpdateAvailable`), so both say the same thing
/// in the same place. `version` is bare (`0.2.0`), `url` the release page
/// (`fauna_core::version::release_page_url`).
pub fn show_update_notice(version: &str, url: &str) {
    let notice = settings::update_available_notice(version, url);
    UPDATE_NOTICE.with_borrow_mut(|(text, slot)| {
        if let Some(label) = slot {
            label.set_label(&notice);
            label.set_visible(true);
        }
        *text = Some(notice);
    });
}

// ---------------------------------------------------------------------------
// Theme persistence — stored as a plain string in ~/.config/fauna/theme.json
// ---------------------------------------------------------------------------

/// Path to `~/.config/fauna/theme.json`.
fn theme_config_path() -> Option<PathBuf> {
    Some(
        crate::window_state::dirs_config()?
            .join("fauna")
            .join("theme.json"),
    )
}

/// Load the persisted colour-scheme preference.
///
/// Returns `adw::ColorScheme::Default` if no preference has been saved yet.
pub fn load_theme_preference() -> adw::ColorScheme {
    let Some(path) = theme_config_path() else {
        return adw::ColorScheme::Default;
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return adw::ColorScheme::Default;
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return adw::ColorScheme::Default;
    };
    match value.get("scheme").and_then(|v| v.as_str()) {
        Some("force-light") => adw::ColorScheme::ForceLight,
        Some("force-dark") => adw::ColorScheme::ForceDark,
        _ => adw::ColorScheme::Default,
    }
}

/// Persist the chosen colour scheme to disk.  Silently ignores errors.
fn save_theme_preference(scheme: adw::ColorScheme) {
    let Some(path) = theme_config_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let label = match scheme {
        adw::ColorScheme::ForceLight => "force-light",
        adw::ColorScheme::ForceDark => "force-dark",
        _ => "default",
    };
    let json = format!("{{\"scheme\":\"{label}\"}}");
    let _ = std::fs::write(&path, json);
}

/// Apply the saved theme preference to the global `AdwStyleManager`.
///
/// Call this once during app startup (after `adw::init()`).
pub fn apply_saved_theme() {
    let scheme = load_theme_preference();
    adw::StyleManager::default().set_color_scheme(scheme);
}
