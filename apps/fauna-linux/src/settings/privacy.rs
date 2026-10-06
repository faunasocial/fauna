use adw::prelude::*;
use fauna_ui_ids as ids;

/// The four `inbox-mode-selector` rows — wire token (which drives the
/// `inbox-mode-{token}` test id) + display label, in canonical button order.
///
/// Shared, not linux's own: this file and tui's `settings/privacy.rs` each
/// carried a byte-identical copy under a comment promising it matched the
/// other. `fauna_protocol::contacts::INBOX_MODES` is that one table now, and
/// it is test-pinned against the shared parser (priority #2).
use fauna_protocol::contacts::INBOX_MODES;

use crate::i18n::strings::common;
use crate::i18n::strings::settings::{self, privacy_page};
use crate::i18n::strings::status::spam;

/// Build the "Privacy" preferences page, plus its nav-edge re-read (the email
/// filters group's — the one part of the page nothing else refreshes).
pub fn build_privacy_page(
    client: &std::rc::Rc<crate::client::FaunaClient>,
) -> (gtk::Box, std::rc::Rc<dyn Fn()>) {
    let page = adw::PreferencesPage::builder()
        .title(privacy_page::TITLE)
        .icon_name("channel-secure-symbolic")
        .build();

    // --- Inbox mode group ---
    let inbox_group = adw::PreferencesGroup::builder()
        .title(settings::INBOX_MODE)
        .description(privacy_page::INBOX_MODE_DESCRIPTION)
        .build();

    // Native gtk::CheckButton radio group — one per mode, each with its own
    // test ID (inbox-mode-open, inbox-mode-allow_knock, …), matching web's
    // `<input type=radio>`. The agent actuates a CheckButton via activate().
    let mode_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    mode_box.set_margin_top(8);
    mode_box.set_margin_bottom(8);

    // error-message — page-level error label (Rule 2), hidden until set. The
    // only producer today is the unknown-inbox-mode explanation below; the
    // agent's `error_text()` falls back to a mapped page-level `error-message`
    // when no global banner is up (`main.rs`), so it is readable from tests.
    let error_label = gtk::Label::builder().visible(false).build();
    error_label.add_css_class("error");
    error_label.set_halign(gtk::Align::Start);
    error_label.set_wrap(true);
    crate::testid::set_test_id(&error_label, ids::ERROR_MESSAGE);
    inbox_group.add(&error_label);

    let mut buttons: Vec<gtk::CheckButton> = Vec::new();
    for (value, label) in INBOX_MODES {
        let btn = gtk::CheckButton::with_label(label);
        let test_id = format!("inbox-mode-{value}");
        crate::testid::set_test_id(&btn, &test_id);
        crate::offline_gate::declare_wire_kind(&btn, "fauna.inbox.mode.set");
        mode_box.append(&btn);
        buttons.push(btn);
    }

    // Link into a radio group so only one is active at a time.
    for i in 1..buttons.len() {
        buttons[i].set_group(Some(&buttons[0]));
    }

    // NO default selection. Until the account's real mode lands, all four paint
    // unmarked and the page says why — tui's ratified shape
    // (`apps/fauna-tui/src/settings/mod.rs::page_error`, which reads
    // `PrivacyState::inbox_mode.is_none()`). The page used to activate "open"
    // here, which is a *claim* about the user's privacy posture that the app had
    // not yet made a single request to check.
    let apply_known_mode = {
        let buttons = buttons.clone();
        let error_label = error_label.clone();
        move |mode: Option<&str>| match mode.and_then(|m| {
            INBOX_MODES
                .iter()
                .position(|(value, _)| *value == m)
                .map(|i| (i, m))
        }) {
            Some((i, _)) => {
                super::render_error_label(&error_label, None);
                buttons[i].set_active(true);
            }
            None => {
                // Unknown, or a mode this build has no radio for (a newer nest);
                // either way marking nothing is the honest paint, and the copy
                // is what turns "nothing selected" from a puzzle into an answer.
                super::render_error_label(&error_label, Some(privacy_page::INBOX_MODE_UNKNOWN));
            }
        }
    };

    // Wire each button's toggled signal to the server call and local state.
    // `applying_remote` suppresses the echo: `set_active()` from the loaded-mode
    // handler below fires `toggled` exactly like a user click would, and without
    // this guard simply *displaying* the stored mode would POST it straight back
    // to the nest — a write the user never asked for, on every page open.
    let applying_remote = std::rc::Rc::new(std::cell::Cell::new(false));
    for (i, btn) in buttons.iter().enumerate() {
        let mode_value = INBOX_MODES[i].0;
        let applying_remote = applying_remote.clone();
        btn.connect_toggled(move |b| {
            if !b.is_active() {
                return; // Only act on the newly-active button.
            }
            if applying_remote.get() {
                return; // Painting what the nest already holds, not a user choice.
            }
            // Track the mode locally so the test agent can expose it in
            // the app state JSON for E2E tests.
            crate::settings::set_inbox_mode(mode_value);
            if let Some(client) = crate::settings::get_client() {
                client.set_inbox_mode(mode_value);
            } else {
                tracing::error!("[settings/privacy] set_inbox_mode: no client available");
            }
        });
    }

    // Paint whatever this process already knows (a mode fetched earlier, or one
    // the user chose before navigating away), then register for the reply of the
    // fetch below. Both go through the same painter, so a page rebuilt after the
    // reply landed is not left blank waiting for an event that already fired.
    {
        let applying_remote = applying_remote.clone();
        let paint = apply_known_mode.clone();
        crate::settings::set_inbox_mode_loaded_handler(std::rc::Rc::new(move |mode: &str| {
            applying_remote.set(true);
            paint(Some(mode));
            applying_remote.set(false);
        }));
    }
    {
        let known = crate::settings::get_inbox_mode();
        applying_remote.set(true);
        apply_known_mode(if known.is_empty() {
            None
        } else {
            Some(known.as_str())
        });
        applying_remote.set(false);
    }

    // Fetch current mode from server and pre-select (via the handler above).
    if let Some(client) = crate::settings::get_client() {
        client.fetch_inbox_mode();
    }

    let mode_row = adw::ActionRow::builder()
        .title(settings::INBOX_MODE)
        .subtitle(privacy_page::INBOX_MODE_SUBTITLE)
        .build();
    mode_row.add_suffix(&mode_box);

    inbox_group.add(&mode_row);
    page.add(&inbox_group);

    // --- Email filters group ---
    // Self-contained module (list/create/edit/delete over the real filter
    // ids) — see settings/email_filters.rs for why this isn't inline here
    // anymore.
    let (filters_group, filters_refresh) =
        crate::settings::email_filters::build_email_filters_group(client);
    page.add(&filters_group);

    // --- Spam thresholds group ---
    let spam_group = adw::PreferencesGroup::builder()
        .title(privacy_page::SPAM_PROTECTION)
        .description(privacy_page::SPAM_PROTECTION_DESCRIPTION)
        .build();
    // E2E test ID for the spam preferences section.
    // adw::PreferencesGroup doesn't reliably surface accessible descriptions
    // via AT-SPI, so we add a zero-height marker label as the header suffix
    // instead.  The marker is discoverable by the AT-SPI bridge and has
    // VISIBLE+SHOWING states because it lives in the group header.
    let spam_marker = gtk::Label::new(None);
    spam_marker.set_height_request(0);
    spam_marker.set_overflow(gtk::Overflow::Hidden);
    crate::testid::set_test_id(&spam_marker, ids::SPAM_PREFERENCES);
    spam_group.set_header_suffix(Some(&spam_marker));

    let (spam_row, spam_scale) = build_scale_row(
        spam::SPAM_THRESHOLD,
        privacy_page::SPAM_THRESHOLD_SUBTITLE,
        0.0,
        1.0,
        0.5, // nest SpamPreferences::default (bins/fauna-nest/src/db/mod.rs)
    );
    // E2E test ID for the spam threshold scale.
    crate::testid::set_test_id(&spam_scale, ids::SPAM_THRESHOLD);
    // Wired to `send_spam_prefs` on every value change below (linux's
    // immediate-apply shape, unlike tui's explicit-Save-only field) — the
    // scale itself issues the same `SaveSpamPrefs` kind, the nostr switches'
    // per-toggle-issues-its-own-write shape.
    crate::offline_gate::declare_wire_kind(&spam_scale, "fauna.spam.set_preferences");

    // Threshold band label (Aggressive / Moderate / Permissive) — the shared
    // mapping from `fauna_protocol::spam` (reached via the `fauna-client-spam`
    // re-export), the single source of truth every app renders from
    // (docs/goal/ui/settings.md § Spam threshold slider labels). Mirrors web's
    // `pref-hint` span; updates on value-changed. No inline buckets, and no test
    // id (web's hint carries none either, so we don't invent a linux-only ID).
    let band_label = gtk::Label::new(Some(spam_band_label(spam_scale.value())));
    band_label.add_css_class("dim-label");
    band_label.set_valign(gtk::Align::Center);
    spam_row.add_suffix(&band_label);
    {
        let band_label = band_label.clone();
        spam_scale.connect_value_changed(move |s| {
            band_label.set_text(spam_band_label(s.value()));
        });
    }

    spam_group.add(&spam_row);

    let (phishing_row, phishing_scale) = build_scale_row(
        spam::PHISHING_THRESHOLD,
        privacy_page::PHISHING_THRESHOLD_SUBTITLE,
        0.0,
        1.0,
        0.3, // nest SpamPreferences::default (bins/fauna-nest/src/db/mod.rs)
    );
    // E2E test ID for the phishing threshold scale.
    crate::testid::set_test_id(&phishing_scale, ids::PHISHING_THRESHOLD);
    crate::offline_gate::declare_wire_kind(&phishing_scale, "fauna.spam.set_preferences");
    spam_group.add(&phishing_row);

    // Fetch current spam preferences.
    if let Some(client) = crate::settings::get_client() {
        client.fetch_spam_preferences();
    }

    // "Save" button for spam preferences — E2E tests click this to persist.
    let save_spam_btn = gtk::Button::with_label(common::SAVE);
    save_spam_btn.add_css_class("suggested-action");
    crate::testid::set_test_id(&save_spam_btn, ids::SAVE_SPAM_PREFS);
    crate::offline_gate::declare_wire_kind(&save_spam_btn, "fauna.spam.set_preferences");
    {
        let spam_scale_ref = spam_scale.clone();
        let phishing_scale_ref = phishing_scale.clone();
        save_spam_btn.connect_clicked(move |_| {
            send_spam_prefs(&spam_scale_ref, &phishing_scale_ref);
        });
    }

    let save_row = adw::ActionRow::builder()
        .title(privacy_page::APPLY_CHANGES)
        .build();
    save_row.add_suffix(&save_spam_btn);
    spam_group.add(&save_row);

    // Also wire scale changes → server update for immediate feedback.
    {
        let spam_scale_ref = spam_scale.clone();
        let phishing_scale_ref = phishing_scale.clone();
        spam_scale.connect_value_changed(move |_| {
            send_spam_prefs(&spam_scale_ref, &phishing_scale_ref);
        });
    }

    page.add(&spam_group);

    (
        crate::testid::wrap_page_with_heading(privacy_page::TITLE, ids::PAGE_HEADING, &page),
        filters_refresh,
    )
}

/// Build a preferences row containing a horizontal scale (slider).
/// Returns the row and a clone of the scale widget for wiring.
fn build_scale_row(
    title: &str,
    subtitle: &str,
    min: f64,
    max: f64,
    default: f64,
) -> (adw::ActionRow, gtk::Scale) {
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(subtitle)
        .build();

    let scale = gtk::Scale::builder()
        .orientation(gtk::Orientation::Horizontal)
        // step_increment 0.1 (the goal-doc-mandated step) + digits(1) so the
        // value reads 0.0–1.0 in tenths — docs/goal/ui/settings.md § Spam
        // threshold slider labels ("the same range/label mapping applies on
        // every app").
        .adjustment(&gtk::Adjustment::new(default, min, max, 0.1, 0.1, 0.0))
        .width_request(200)
        .valign(gtk::Align::Center)
        .draw_value(true)
        .digits(1)
        .build();

    row.add_suffix(&scale);
    (row, scale)
}

/// Localized threshold-band label ("Aggressive" / "Moderate" / "Permissive") for
/// a probability value in `[0.0, 1.0]`, via the shared `fauna_protocol::spam`
/// band mapping (re-exported through `fauna-client-spam`). Both the bucket logic
/// **and** the band → label map are shared Rust — the single source of truth
/// every app renders from (docs/goal/ui/settings.md § Spam threshold slider
/// labels); no inline buckets and no inline label map.
fn spam_band_label(value: f64) -> &'static str {
    fauna_client_spam::spam::spam_band_label(value)
}

/// Collect all spam preference values and send them to the server.
fn send_spam_prefs(spam_scale: &gtk::Scale, phishing_scale: &gtk::Scale) {
    use fauna_client_spam::spam::probability_to_per_mille;
    // GTK scales work in probability `[0.0, 1.0]`; the wire carries per-mille
    // `u16` (0–1000) — the dag-cbor wire forbids floats (`fauna_protocol::spam`).
    // The probability→per-mille conversion is the shared `fauna_protocol::spam`
    // fn (re-exported via `fauna-client-spam`) every app renders from.
    let req = fauna_client_spam::spam::SpamSetPreferencesRequest {
        spam_threshold: Some(probability_to_per_mille(spam_scale.value())),
        phishing_threshold: Some(probability_to_per_mille(phishing_scale.value())),
        extra: Default::default(),
    };
    if let Some(client) = crate::settings::get_client() {
        client.update_spam_preferences(req);
    }
}

// Client-local Bayesian spam classifier (bayes_model_path,
// load/save_bayes_model_to_disk, apply_read_result) removed — zero production callers, superseded by the real on-device
// "Fauna-app" scoring position, `libs/fauna-client-mail-settings::inbox_scorer`,
// which scores against the shared `fauna_mail::spam::SpamModel`.
