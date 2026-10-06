//! The user-facing "Mail" preferences page (I3 Phase C, linux).
//!
//! Single touchpoint for managing third-party-MUA (Thunderbird, Apple Mail)
//! access to the account: enable/disable mail, see and revoke credentials,
//! rotate the underlying MSEK, and read the MUA connection details. Target
//! state: `docs/goal/ui/mail-settings.md`; feature behavior:
//! `docs/goal/behavior/mail-credentials.md`.
//!
//! # Slice scope
//!
//! Slices 1–2 wired the static skeleton + the shared
//! `fauna_client_mail_settings::MailSettingsMachine` (enable flow). Slice 3
//! adds the credential lifecycle UI: the `mail-add-credential` inline form
//! (name + kind selector + PLAIN password / OAUTHBEARER one-time-token) and
//! soft-revoke — the app-password rows themselves now list on Settings →
//! Connected apps. Per mail-settings.md
//! § Where logic lives, this layer holds **no** business logic — it is dumb
//! rendering of `MailSettingsSnapshot` + dispatch of `MailSettingsAction`.
//!
//! Enable is the "first credential" case: per mail-settings.md § User actions,
//! the enabled-toggle opens the same add-credential form, and submit dispatches
//! `EnableMail` (vs `AddCredential` once mail is already on). Slice 4 wires
//! rotate-keys + the pending-rotation banner.
//!
//! # AT-SPI discoverability (why bare gtk widgets, not adw rows)
//!
//! `adw::PreferencesGroup`/`ActionRow` titles/subtitles and `adw::ComboRow`/
//! `SwitchRow` do not reliably surface their accessible name/description to
//! AT-SPI under Mutter, and a `GtkDropDown`'s selection can't be driven
//! coordinate-free in a headless session (a known coordinate-degraded-automation
//! limitation). So: read-only fields carry
//! a 1px marker `gtk::Label` tagged with `set_test_id`; directly-actuable
//! controls (`gtk::Entry`, `gtk::DropDown`, `gtk::Button`, `gtk::ToggleButton`)
//! carry the ID on the widget itself and are actuated by their own AT-SPI
//! action — the same idiom privacy.rs's email-filter form uses. The
//! add-credential surface is an **inline reveal** (not a modal): the linux
//! state protocol can't open the separate `adw::PreferencesWindow`, so the
//! whole page is embedded in the status view (`views/status.rs`); an inline
//! form stays inside that reachable tree.

use fauna_ui_ids as ids;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;

use fauna_client_mail_settings::{
    CredentialKind, MailSettingsAction, MailSettingsMachine, MailSettingsSnapshot, SecretBytes,
    SettingsStatus,
};

use crate::async_helper::spawn_with_snapshot;
use crate::i18n::strings::common;
use crate::i18n::strings::mail_settings as MS;
use crate::i18n::strings::settings::mail as SM;
use crate::testid::{set_test_attr, set_test_id};

/// Which dispatch the add-credential form's submit maps to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FormMode {
    /// First credential on a never-enabled actor → `EnableMail`.
    Enable,
    /// New credential on an already-enabled actor → `AddCredential`.
    Add,
}

/// Handles to the widgets the snapshot renders into. GTK objects are
/// reference-counted, so cloning this is cheap and shares the same widgets.
#[derive(Clone)]
struct MailPageWidgets {
    // Top group.
    enabled_toggle: adw::SwitchRow,
    status_marker: gtk::Label,
    status_row: adw::ActionRow,
    error_label: gtk::Label,

    // Pending-rotation banner.
    banner_group: adw::PreferencesGroup,
    resume_btn: gtk::Button,

    // The one-line pointer to Settings → Connected apps, where the app
    // passwords now list (`mail-settings.md` § Where the credential rows render).
    credentials_group: adw::PreferencesGroup,

    // Manage group (add + rotate buttons).
    manage_group: adw::PreferencesGroup,
    add_credential_btn: gtk::Button,
    rotate_keys_btn: gtk::Button,

    // Add-credential inline form.
    form_group: adw::PreferencesGroup,
    form_input_box: gtk::Box,
    name_input: gtk::Entry,
    type_selector: gtk::CheckButton,
    plain_box: gtk::Box,
    autogenerate_toggle: gtk::CheckButton,
    password_input: gtk::Entry,
    password_show_toggle: gtk::ToggleButton,
    password_strength: gtk::Label,
    weak_password_warning: gtk::Label,
    token_box: gtk::Box,
    token_display: gtk::Label,
    token_copy_btn: gtk::Button,
    submit_btn: gtk::Button,
    cancel_btn: gtk::Button,

    // Rotate-keys inline form (mail-rotate-keys-confirm).
    rotate_form_group: adw::PreferencesGroup,
    rotate_exclude_box: gtk::Box,
    rotate_exclude_checks: Rc<RefCell<Vec<(String, gtk::CheckButton)>>>,
    rotate_progress: gtk::Label,
    rotate_confirm_btn: gtk::Button,
    rotate_cancel_btn: gtk::Button,
    /// `Some(credentials to re-wrap)` while a confirmed rotation is still
    /// running — set on confirm, cleared when the rotation's own dispatch
    /// returns. While set, the rotate form stays open with its progress line
    /// painted and its controls disabled, so the multi-step rotation is visible
    /// while it runs and cannot be started twice (`mail-settings.md` § Element
    /// visibility) — the tui and web shape.
    rotation_in_flight: Rc<Cell<Option<u64>>>,

    // MUA instructions group. The IMAP/SMTP rows describe the email protocol and
    // render when email is enabled; the CalDAV rows describe the calendar protocol
    // and render when CalDAV is enabled (the two enable independently —
    // caldav-server.md § Independent enablement); the WebDAV row is a single URL
    // (no host/port split — no SRV autodiscovery exists for WebDAV) gated on
    // `serves_webdav_set` (the actor serving >=1 folder over WebDAV). Username +
    // AUTH are shared by all three, so they show whenever the group does. The
    // per-protocol row handles are held so render() can gate each protocol's rows
    // individually within the one `mail-settings-mua-instructions` group.
    mua_group: adw::PreferencesGroup,
    mua_imap_host_row: adw::ActionRow,
    mua_imap_host: gtk::Label,
    mua_imap_port_row: adw::ActionRow,
    mua_imap_port: gtk::Label,
    mua_smtp_host_row: adw::ActionRow,
    mua_smtp_host: gtk::Label,
    mua_smtp_port_row: adw::ActionRow,
    mua_smtp_port: gtk::Label,
    mua_caldav_host_row: adw::ActionRow,
    mua_caldav_host: gtk::Label,
    mua_caldav_port_row: adw::ActionRow,
    mua_caldav_port: gtk::Label,
    mua_webdav_url_row: adw::ActionRow,
    mua_webdav_url: gtk::Label,
    mua_username: gtk::Label,
    mua_auth: gtk::Label,

    // Local IMAP/CalDAV-serving toggle group (visible when enabled).
    serve_here_group: adw::PreferencesGroup,
    serve_here_toggle: adw::SwitchRow,
}

/// Everything the page's event handlers + render need: the shared machine, the
/// tokio handle for async dispatch, the toggle re-entrancy guard, the current
/// form mode, and the widget handles. `Rc`-shared into every closure.
struct MailPageCtx {
    machine: Arc<MailSettingsMachine>,
    rt: tokio::runtime::Handle,
    guard: Rc<Cell<bool>>,
    form_mode: Cell<FormMode>,
    w: MailPageWidgets,
}

/// Build the "Mail & Calendar" preferences page.
pub fn build_mail_page() -> adw::PreferencesPage {
    // Ampersand handling: AdwPreferencesPage.title is NOT Pango markup (the bare
    // "&" renders literally), but AdwPreferencesGroup.title IS markup — empirically
    // it emits `Gtk-WARNING: Failed to set text … from markup` on a bare "&" (the
    // Adw GIR omits this, but the runtime warns; same root cause as the
    // AdwPreferencesRow `serve_here_toggle` set_use_markup(false) below). Groups
    // expose no use-markup property, so escape "&" → "&amp;" in group titles; the
    // page title stays raw.
    let page = adw::PreferencesPage::builder()
        .title(SM::SECTION_TITLE)
        .icon_name("mail-unread-symbolic")
        .build();

    // --- Top group: heading + enable toggle + status + page-level error ---
    let top_group = adw::PreferencesGroup::builder()
        .title(glib::markup_escape_text(SM::SECTION_TITLE).as_str())
        .description(SM::SECTION_DESCRIPTION)
        .build();

    // page-heading — global-rule heading element. A marker label so AT-SPI
    // resolves the ID; the visible group title carries the human-readable text.
    top_group.set_header_suffix(Some(&super::marker("page-heading")));

    // mail-settings-enabled-toggle — a native adw::SwitchRow. The in-process
    // agent actuates a Switch via set_active, so no ToggleButton work-around is
    // needed; the separate status row below mirrors the on/off state.
    let enabled_toggle = adw::SwitchRow::builder()
        .title(SM::ENABLE_TITLE)
        .subtitle(SM::ENABLE_SUBTITLE)
        .active(false)
        .build();
    set_test_id(&enabled_toggle, ids::MAIL_SETTINGS_ENABLED_TOGGLE);
    // The toggle is a composite: ON opens the add-credential form, whose own
    // `mail-add-credential-submit-button` already declares the kind that binds
    // that leg (`MailAddCredentialSubmit`). OFF's ceremony instead dead-ends at
    // an `adw::AlertDialog` confirm button (via `crate::confirm_dialog`) with no
    // persistent widget reference — the same shape as `admin.rs::build_factory_reset_section`'s
    // `ADMIN_FACTORY_RESET_BUTTON` — so the ENTRY is what declares the OFF
    // leg's kind (`MailDisableConfirm`'s `DisableMail`, an OfflineSafe
    // `fauna.state.mail` write, not a bridge call).
    crate::offline_gate::declare_wire_kind(&enabled_toggle, "fauna.account.state.put");
    top_group.add(&enabled_toggle);

    // mail-settings-status-indicator — subtle status line. The marker label
    // carries the status text for the e2e driver; the row subtitle mirrors it.
    let status_marker = super::marker("mail-settings-status-indicator");
    let status_row = adw::ActionRow::builder()
        .title(common::STATUS)
        .subtitle(SM::STATUS_DISABLED)
        .build();
    status_row.add_suffix(&status_marker);
    top_group.add(&status_row);

    // error-message — page-level error label (Rule 2), hidden until set.
    let error_label = gtk::Label::builder().visible(false).build();
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    let error_row = adw::ActionRow::builder().activatable(false).build();
    error_row.add_suffix(&error_label);
    top_group.add(&error_row);

    page.add(&top_group);

    // --- Pending-rotation banner (shown when snapshot.pending_rotation set) ---
    let banner_group = adw::PreferencesGroup::new();
    banner_group.set_visible(false);
    let banner_row = adw::ActionRow::builder()
        .title(SM::BANNER_TITLE)
        .subtitle(SM::BANNER_SUBTITLE)
        .build();
    banner_row.add_prefix(&gtk::Image::from_icon_name("dialog-warning-symbolic"));
    banner_row.add_suffix(&super::marker("mail-settings-pending-rotation-banner"));
    let resume_btn = gtk::Button::builder()
        .label(SM::RESUME)
        .valign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .build();
    set_test_id(
        &resume_btn,
        ids::MAIL_SETTINGS_PENDING_ROTATION_RESUME_BUTTON,
    );
    crate::offline_gate::declare_wire_kind(&resume_btn, "fauna.bridges.provision_wrapped_mls_blob");
    banner_row.add_suffix(&resume_btn);
    banner_group.add(&banner_row);
    page.add(&banner_group);

    // --- Credentials pointer group ---
    // The app-password rows (`mail-settings-credential-item*`) moved to
    // Settings → Connected apps; this group keeps the one line that says so.
    // Visible when a mailbox exists.
    let credentials_group = adw::PreferencesGroup::builder()
        .title(SM::CREDENTIALS_TITLE)
        .build();
    let credentials_pointer = gtk::Label::builder()
        .label(MS::CREDENTIALS_ON_CONNECTED_APPS)
        .wrap(true)
        .xalign(0.0)
        .css_classes(["dim-label"])
        .build();
    credentials_group.add(&credentials_pointer);
    credentials_group.set_visible(false);
    page.add(&credentials_group);

    // --- Manage group: add-credential + rotate-keys buttons ---
    let manage_group = adw::PreferencesGroup::new();
    manage_group.set_visible(false);

    // mail-settings-add-credential-button — visible when enabled.
    let add_credential_btn = gtk::Button::builder()
        .label(SM::ADD_CREDENTIAL)
        .valign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .build();
    set_test_id(
        &add_credential_btn,
        ids::MAIL_SETTINGS_ADD_CREDENTIAL_BUTTON,
    );
    let add_row = adw::ActionRow::builder().activatable(false).build();
    add_row.add_suffix(&add_credential_btn);
    manage_group.add(&add_row);

    // mail-settings-rotate-keys-button — visible when enabled + ≥1 credential.
    let rotate_keys_btn = gtk::Button::builder()
        .label(SM::ROTATE_KEYS)
        .valign(gtk::Align::Center)
        .css_classes(["destructive-action"])
        .build();
    set_test_id(&rotate_keys_btn, ids::MAIL_SETTINGS_ROTATE_KEYS_BUTTON);
    // mail-settings-keys-info — the (i) explainer beside the rotate-keys button:
    // what the keys protect, when to rotate (suspected compromise) vs revoke one
    // retired credential, and that already-received mail stays readable. The
    // shared copy (S.mail_settings.keys_info) is the rotate row's subtitle (the
    // user-visible rationale) and a 1px marker carries it for the e2e driver.
    let rotate_row = adw::ActionRow::builder()
        .activatable(false)
        .title(SM::KEYS_TITLE)
        .subtitle(MS::KEYS_INFO)
        .build();
    rotate_row.add_prefix(&gtk::Image::from_icon_name("dialog-information-symbolic"));
    rotate_row.add_suffix(&super::value_marker(
        "mail-settings-keys-info",
        MS::KEYS_INFO,
    ));
    rotate_row.add_suffix(&rotate_keys_btn);
    manage_group.add(&rotate_row);
    page.add(&manage_group);

    // --- Add-credential inline form (mail-add-credential; hidden) ---
    // An inline reveal rather than a modal — see the module docs. The form
    // reuses the page's `page-heading` and `error-message` (per ui-actual-
    // linux.yaml, those are not listed missing for the mail-add-credential
    // page; the inline form shares the page's generic ones).
    let form_group = adw::PreferencesGroup::builder()
        .title(SM::ADD_TITLE)
        .build();
    form_group.set_visible(false);

    let form_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    form_box.set_margin_top(8);
    form_box.set_margin_bottom(8);

    // The input section (name + kind + PLAIN fields + submit). Hidden once an
    // OAUTHBEARER token is shown (the form switches to the token reveal).
    let form_input_box = gtk::Box::new(gtk::Orientation::Vertical, 8);

    // mail-add-credential-name-input.
    let name_input = gtk::Entry::builder()
        .placeholder_text(SM::NAME_PLACEHOLDER)
        .build();
    set_test_id(&name_input, ids::MAIL_ADD_CREDENTIAL_NAME_INPUT);
    form_input_box.append(&name_input);

    // mail-add-credential-type-selector — credential-kind picker. A single
    // native gtk::CheckButton with one stable id (value-via-state, per
    // mail-settings.md § the singular selector decision): unchecked =
    // OAUTHBEARER (the recommended default per `mail-credentials.md` § KDF
    // choice), checked = PLAIN, which reveals the password fields. It is the
    // binary-checkbox form of the catalog's "pick-one" intent — a 2-element
    // radio group would split the single e2e id (`click` flips Bearer↔PLAIN)
    // for no clarity gain, and there is no web counterpart to match.
    let type_selector = gtk::CheckButton::with_label(SM::TYPE_SELECTOR);
    set_test_id(&type_selector, ids::MAIL_ADD_CREDENTIAL_TYPE_SELECTOR);
    form_input_box.append(&type_selector);

    // PLAIN-only fields: auto-generate toggle + password entry + show toggle +
    // strength readout + weak-password warning.
    let plain_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    plain_box.set_visible(false);

    // mail-add-credential-autogenerate-toggle — DEFAULT ON (mail-credentials.md
    // § Auto-generated bridge password). When on, the password input holds a
    // client-minted ~143-bit secret (generate_bridge_password()) and is
    // read-only (shown-once to copy into the MUA); turning it off enables manual
    // entry. The fill/read-only wiring lives in `apply_autogen_state`.
    let autogenerate_toggle = gtk::CheckButton::with_label(SM::AUTOGENERATE);
    autogenerate_toggle.set_active(true);
    set_test_id(
        &autogenerate_toggle,
        ids::MAIL_ADD_CREDENTIAL_AUTOGENERATE_TOGGLE,
    );
    plain_box.append(&autogenerate_toggle);

    let pw_hbox = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    // mail-add-credential-password-input.
    let password_input = gtk::Entry::builder()
        .placeholder_text(SM::PASSWORD_PLACEHOLDER)
        .visibility(false)
        .hexpand(true)
        .build();
    set_test_id(&password_input, ids::MAIL_ADD_CREDENTIAL_PASSWORD_INPUT);
    pw_hbox.append(&password_input);
    // mail-add-credential-password-show-toggle.
    let password_show_toggle = gtk::ToggleButton::builder().label(SM::SHOW).build();
    set_test_id(
        &password_show_toggle,
        ids::MAIL_ADD_CREDENTIAL_PASSWORD_SHOW_TOGGLE,
    );
    pw_hbox.append(&password_show_toggle);
    plain_box.append(&pw_hbox);

    // mail-add-credential-password-strength-meter — advisory text readout.
    let password_strength = gtk::Label::builder()
        .label("")
        .halign(gtk::Align::Start)
        .css_classes(["dim-label"])
        .build();
    set_test_id(
        &password_strength,
        ids::MAIL_ADD_CREDENTIAL_PASSWORD_STRENGTH_METER,
    );
    plain_box.append(&password_strength);

    // mail-add-credential-weak-password-warning — shown only when auto-generate
    // is OFF (manual entry weakens at-rest security: the password Argon2id-wraps
    // the MLS capability). Hidden by default (auto-generate defaults ON).
    // TODO(Feature C): gate additionally on an encrypted nest via
    // password_gen::warn_manual_password once MailSettingsSnapshot carries the
    // storage mode (today the page has no nest-encrypted signal; on the
    // encrypted dogfood deployments this manual-only warning is already correct).
    let weak_password_warning = gtk::Label::builder()
        .label(SM::WEAK_PASSWORD_WARNING)
        .wrap(true)
        .halign(gtk::Align::Start)
        .css_classes(["warning"])
        .visible(false)
        .build();
    set_test_id(
        &weak_password_warning,
        ids::MAIL_ADD_CREDENTIAL_WEAK_PASSWORD_WARNING,
    );
    plain_box.append(&weak_password_warning);
    form_input_box.append(&plain_box);

    // mail-add-credential-submit-button.
    let submit_btn = gtk::Button::builder()
        .label(SM::SUBMIT_ADD)
        .css_classes(["suggested-action"])
        .build();
    set_test_id(&submit_btn, ids::MAIL_ADD_CREDENTIAL_SUBMIT_BUTTON);
    // Both submit arms — first credential (Enable) and subsequent (Add) — end in
    // the same provisioning write, so the add-vs-enable mode this button does
    // not carry never changes the kind (mirrors tui's `MailAddCredentialSubmit`
    // ruling).
    crate::offline_gate::declare_wire_kind(&submit_btn, "fauna.bridges.provision_wrapped_mls_blob");
    form_input_box.append(&submit_btn);
    form_box.append(&form_input_box);

    // OAUTHBEARER one-time token reveal (shown after a successful submit).
    let token_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    token_box.set_visible(false);
    let token_warning = gtk::Label::builder()
        .label(SM::TOKEN_WARNING)
        .halign(gtk::Align::Start)
        .wrap(true)
        .css_classes(["dim-label"])
        .build();
    token_box.append(&token_warning);
    // mail-add-credential-token-display.
    let token_display = gtk::Label::builder()
        .label("")
        .selectable(true)
        .wrap(true)
        .halign(gtk::Align::Start)
        .css_classes(["monospace"])
        .build();
    set_test_id(&token_display, ids::MAIL_ADD_CREDENTIAL_TOKEN_DISPLAY);
    token_box.append(&token_display);
    // mail-add-credential-token-copy-button.
    let token_copy_btn = gtk::Button::builder().label(SM::COPY_TOKEN).build();
    set_test_id(&token_copy_btn, ids::MAIL_ADD_CREDENTIAL_TOKEN_COPY_BUTTON);
    token_box.append(&token_copy_btn);
    form_box.append(&token_box);

    // mail-add-credential-cancel-button.
    let cancel_btn = gtk::Button::builder().label(SM::CANCEL).build();
    set_test_id(&cancel_btn, ids::MAIL_ADD_CREDENTIAL_CANCEL_BUTTON);
    form_box.append(&cancel_btn);

    form_group.add(&form_box);
    page.add(&form_group);

    // --- Rotate-keys inline form (mail-rotate-keys-confirm; hidden) ---
    // Inline reveal (not a modal), same rationale as the add-credential form.
    // Opened by the rotate-keys button; confirm dispatches StartRotation with
    // the credentials the user checked as compromised.
    let rotate_form_group = adw::PreferencesGroup::builder()
        .title(SM::ROTATE_TITLE)
        .build();
    rotate_form_group.set_visible(false);
    let rotate_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    rotate_box.set_margin_top(8);
    rotate_box.set_margin_bottom(8);

    // mail-rotate-keys-warning-text — canned compromise/resumable warning.
    let rotate_warning = gtk::Label::builder()
        .label(SM::ROTATE_WARNING)
        .halign(gtk::Align::Start)
        .wrap(true)
        .build();
    set_test_id(&rotate_warning, ids::MAIL_ROTATE_KEYS_WARNING_TEXT);
    rotate_box.append(&rotate_warning);

    let rotate_exclude_caption = gtk::Label::builder()
        .label(SM::ROTATE_EXCLUDE_CAPTION)
        .halign(gtk::Align::Start)
        .css_classes(["dim-label"])
        .build();
    rotate_box.append(&rotate_exclude_caption);

    // mail-rotate-keys-exclude-list — container for one checkbox per credential,
    // rebuilt when the form opens (see open_rotate_form). The container carries
    // the ID; per-credential checkboxes have no separate ui.yaml ID.
    let rotate_exclude_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
    set_test_id(&rotate_exclude_box, ids::MAIL_ROTATE_KEYS_EXCLUDE_LIST);
    rotate_box.append(&rotate_exclude_box);

    // mail-rotate-keys-progress-indicator — reflects status::RotationInProgress.
    let rotate_progress = gtk::Label::builder()
        .label("")
        .halign(gtk::Align::Start)
        .css_classes(["dim-label"])
        .build();
    set_test_id(&rotate_progress, ids::MAIL_ROTATE_KEYS_PROGRESS_INDICATOR);
    rotate_box.append(&rotate_progress);

    // mail-rotate-keys-confirm-button.
    let rotate_confirm_btn = gtk::Button::builder()
        .label(SM::ROTATE_CONFIRM)
        .css_classes(["destructive-action"])
        .build();
    set_test_id(&rotate_confirm_btn, ids::MAIL_ROTATE_KEYS_CONFIRM_BUTTON);
    crate::offline_gate::declare_wire_kind(
        &rotate_confirm_btn,
        "fauna.bridges.provision_wrapped_mls_blob",
    );
    rotate_box.append(&rotate_confirm_btn);

    // mail-rotate-keys-cancel-button.
    let rotate_cancel_btn = gtk::Button::builder().label(SM::CANCEL).build();
    set_test_id(&rotate_cancel_btn, ids::MAIL_ROTATE_KEYS_CANCEL_BUTTON);
    rotate_box.append(&rotate_cancel_btn);

    rotate_form_group.add(&rotate_box);
    page.add(&rotate_form_group);

    // --- MUA setup instructions group (read-only; visible when a mailbox exists) ---
    let mua_group = adw::PreferencesGroup::builder()
        // "&" escaped — AdwPreferencesGroup.title is Pango markup (see build_mail_page top).
        .title(glib::markup_escape_text(SM::MUA_TITLE).as_str())
        .description(SM::MUA_DESCRIPTION)
        .build();
    mua_group.set_header_suffix(Some(&super::marker("mail-settings-mua-instructions")));
    mua_group.set_visible(false);

    let (imap_host_row, mua_imap_host) = mua_field_row(
        SM::MUA_IMAP_HOST,
        "mail.[your-domain]",
        "mail-settings-mua-imap-host",
    );
    mua_group.add(&imap_host_row);
    let (imap_port_row, mua_imap_port) =
        mua_field_row(SM::MUA_IMAP_PORT, "993", "mail-settings-mua-imap-port");
    mua_group.add(&imap_port_row);
    let (smtp_host_row, mua_smtp_host) = mua_field_row(
        SM::MUA_SMTP_HOST,
        "mail.[your-domain]",
        "mail-settings-mua-smtp-host",
    );
    mua_group.add(&smtp_host_row);
    let (smtp_port_row, mua_smtp_port) =
        mua_field_row(SM::MUA_SMTP_PORT, "465", "mail-settings-mua-smtp-port");
    mua_group.add(&smtp_port_row);
    // CalDAV (calendar) connection detail — mail.<domain>:443 (caldav-server.md
    // § Network exposure). Gated on CalDAV being enabled, independent of email.
    let (caldav_host_row, mua_caldav_host) = mua_field_row(
        SM::MUA_CALDAV_HOST,
        "mail.[your-domain]",
        "mail-settings-mua-caldav-host",
    );
    mua_group.add(&caldav_host_row);
    let (caldav_port_row, mua_caldav_port) =
        mua_field_row(SM::MUA_CALDAV_PORT, "443", "mail-settings-mua-caldav-port");
    mua_group.add(&caldav_port_row);
    // WebDAV (files) connection detail — one full collection-root URL, not a
    // host/port pair (no SRV autodiscovery exists for WebDAV). Gated on
    // `serves_webdav_set` (this actor serves >=1 folder over WebDAV),
    // independent of email/CalDAV/CardDAV.
    let (webdav_url_row, mua_webdav_url) = mua_field_row(
        SM::MUA_WEBDAV_URL,
        "https://mail.[your-domain]/webdav/",
        "mail-settings-mua-webdav-url",
    );
    mua_group.add(&webdav_url_row);
    let (username_row, mua_username) = mua_field_row(
        SM::MUA_USERNAME,
        "[handle]+[credential-id]@[your-domain]",
        "mail-settings-mua-username-format",
    );
    mua_group.add(&username_row);
    let (auth_row, mua_auth) = mua_field_row(
        SM::MUA_AUTH,
        "OAUTHBEARER or PLAIN",
        "mail-settings-mua-auth-mechanism",
    );
    mua_group.add(&auth_row);
    page.add(&mua_group);

    // --- Local IMAP/CalDAV-serving toggle (visible when enabled) ---
    // mail-settings-serve-here-toggle — user-set, default on. Turning it off
    // tells *this* nest's MDA not to serve the user's mailbox over IMAP/CalDAV
    // (the private-home-behind-public-relay deployment). A native adw::SwitchRow,
    // actuated by the agent via set_active; its on/off is carried for the e2e
    // driver via the `state` attr in render(). See mail-settings.md
    // § Local IMAP/CalDAV-serving toggle.
    let serve_here_group = adw::PreferencesGroup::new();
    serve_here_group.set_visible(false);
    // i18n strings are plain text shared by all 7 apps and may contain bare
    // `&`/`<`/`>` (SERVE_HERE_LABEL has "mail & calendar"). AdwPreferencesRow
    // defaults `use-markup=true`, which would break Pango parsing of the title.
    // Disable markup *before* setting the title — applying the title through the
    // builder parses it as markup at `.build()` time and emits the Gtk-WARNING
    // before any later `set_use_markup(false)` could take effect. (Mirrors the
    // `set_use_markup(false)` prior art on plain labels across this area.) Never
    // escape in en.yaml; other apps render the same strings in non-markup
    // contexts where `&amp;` would show literally.
    let serve_here_toggle = adw::SwitchRow::builder().active(true).build();
    serve_here_toggle.set_use_markup(false);
    serve_here_toggle.set_title(MS::SERVE_HERE_LABEL);
    serve_here_toggle.set_subtitle(MS::SERVE_HERE_SUBTITLE);
    set_test_id(&serve_here_toggle, ids::MAIL_SETTINGS_SERVE_HERE_TOGGLE);
    crate::offline_gate::declare_wire_kind(
        &serve_here_toggle,
        "fauna.bridges.set_mail_serving_enabled",
    );
    serve_here_group.add(&serve_here_toggle);
    page.add(&serve_here_group);

    // --- Wire the shared state machine ---
    let widgets = MailPageWidgets {
        enabled_toggle,
        status_marker,
        status_row,
        error_label,
        banner_group,
        resume_btn,
        credentials_group,
        manage_group,
        add_credential_btn,
        rotate_keys_btn,
        form_group,
        form_input_box,
        name_input,
        type_selector,
        plain_box,
        autogenerate_toggle,
        password_input,
        password_show_toggle,
        password_strength,
        weak_password_warning,
        token_box,
        token_display,
        token_copy_btn,
        submit_btn,
        cancel_btn,
        rotate_form_group,
        rotate_exclude_box,
        rotate_exclude_checks: Rc::new(RefCell::new(Vec::new())),
        rotate_progress,
        rotate_confirm_btn,
        rotate_cancel_btn,
        rotation_in_flight: Rc::new(Cell::new(None)),
        mua_group,
        mua_imap_host_row: imap_host_row,
        mua_imap_host,
        mua_imap_port_row: imap_port_row,
        mua_imap_port,
        mua_smtp_host_row: smtp_host_row,
        mua_smtp_host,
        mua_smtp_port_row: smtp_port_row,
        mua_smtp_port,
        mua_caldav_host_row: caldav_host_row,
        mua_caldav_host,
        mua_caldav_port_row: caldav_port_row,
        mua_caldav_port,
        mua_webdav_url_row: webdav_url_row,
        mua_webdav_url,
        mua_username,
        mua_auth,
        serve_here_group,
        serve_here_toggle,
    };
    wire_machine(&page, widgets);

    page
}

/// Build one read-only MUA-instructions row: a title, a subtitle holding the
/// value, and a 1px marker carrying the field's test ID. Returns the row and
/// the value marker label so the caller can update it from the snapshot.
fn mua_field_row(title: &str, value: &str, id: &str) -> (adw::ActionRow, gtk::Label) {
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(value)
        .build();
    let value_marker = gtk::Label::new(Some(value));
    value_marker.set_use_markup(false);
    value_marker.set_height_request(1);
    value_marker.set_overflow(gtk::Overflow::Hidden);
    set_test_id(&value_marker, id);
    row.add_suffix(&value_marker);
    (row, value_marker)
}

/// Connect the page to the shared `MailSettingsMachine`, hydrate on mount, and
/// wire every interaction (toggle, add-credential form, revoke). No-op (page
/// stays at static placeholders) when no client is available — e.g. the unit
/// test, which has no registered client.
fn wire_machine(page: &adw::PreferencesPage, widgets: MailPageWidgets) {
    let client = match crate::settings::get_client() {
        Some(c) => c,
        None => return,
    };
    let machine = match crate::mail_glue::build_mail_settings_machine(&client) {
        Ok(m) => Arc::new(m),
        Err(e) => {
            tracing::error!("mail-settings: build machine failed: {e}");
            return;
        }
    };

    // The Connected apps roster lists this machine's app passwords.
    crate::settings::set_mail_machine(&machine);

    let ctx = Rc::new(MailPageCtx {
        machine,
        rt: client.runtime_handle(),
        guard: Rc::new(Cell::new(false)),
        form_mode: Cell::new(FormMode::Enable),
        w: widgets,
    });

    // Hydrate on mount: load the `fauna.state.mail` entries and render the snapshot.
    hydrate_and_render(&ctx);

    // Re-hydrate whenever the page is shown (navigated to). The settings sub-stack
    // is built once at app launch (`build_settings_view`) and only swaps the
    // visible child on navigation, so a mail-state change made *after* build —
    // notably the background non-admin first-setup auto-mint
    // (the post-claim serving-enablement step's mail provision, which mints on a separate machine) — is
    // invisible to this page's build-time snapshot until it re-fetches. Without
    // this the auto-minted credential (and its one-time generated password,
    // mail-credentials.md § Auto-enable) never surfaces in the session that minted
    // it. Mirrors the `connect_map` refresh-on-show in `logs.rs`.
    {
        let ctx = Rc::clone(&ctx);
        page.connect_map(move |_| hydrate_and_render(&ctx));
    }

    // Re-hydrate when the post-succession aftermath's mail burn settles: it
    // marks every pre-succession credential revoked on the account's mail
    // custody, through a machine of its own, so a page already on screen would
    // keep painting them live until shown again
    // (`crate::settings::set_mail_burn_settled_handler`).
    {
        let ctx = Rc::clone(&ctx);
        crate::settings::set_mail_burn_settled_handler(Rc::new(move || hydrate_and_render(&ctx)));
    }

    // Enable toggle: ON from disabled → open the form in Enable mode. OFF →
    // confirm via a destructive dialog, then `DisableMail` (mail-settings.md
    // § Disable mail: revoke every credential + clear the MSEK). The toggle is
    // snapped back ON immediately so it never shows a lying "off" before the
    // user confirms; the dialog is the real decision point. On confirm the
    // `DisableMail` re-render lands the toggle off; on cancel it stays on.
    {
        let ctx = Rc::clone(&ctx);
        let toggle = ctx.w.enabled_toggle.clone();
        toggle.connect_active_notify(move |btn| {
            if ctx.guard.get() {
                return; // Programmatic sync from render(); not a user action.
            }
            if btn.is_active() {
                open_form(&ctx, FormMode::Enable);
            } else {
                // Restore the on-state without re-firing this handler; the
                // dialog decides whether mail actually gets disabled.
                ctx.guard.set(true);
                btn.set_active(true);
                ctx.guard.set(false);
                open_disable_confirm(&ctx);
            }
        });
    }

    // Add-credential button → open the form in Add mode.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.add_credential_btn.clone().connect_clicked(move |_| {
            open_form(&ctx, FormMode::Add);
        });
    }

    // Local IMAP/CalDAV-serving toggle → SetServingEnabled for this actor.
    // Guarded against the programmatic set_active in render() (same guard the
    // enabled-toggle uses); a genuine user flip dispatches the caller-scoped
    // set_mail_serving_enabled and re-renders from the resulting snapshot.
    {
        let ctx = Rc::clone(&ctx);
        let toggle = ctx.w.serve_here_toggle.clone();
        toggle.connect_active_notify(move |btn| {
            if ctx.guard.get() {
                return; // Programmatic sync from render(); not a user action.
            }
            dispatch_action(
                &ctx,
                MailSettingsAction::SetServingEnabled {
                    enabled: btn.is_active(),
                },
            );
        });
    }

    // Type selector → checked = PLAIN (reveal the password fields),
    // unchecked = OAUTHBEARER (hide them). Revealing PLAIN (re)applies the
    // auto-generate state so the password field is pre-filled with a generated
    // secret when the toggle is on (its default).
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.type_selector.clone().connect_toggled(move |btn| {
            ctx.w.plain_box.set_visible(btn.is_active());
            if btn.is_active() {
                apply_autogen_state(&ctx.w);
            }
        });
    }

    // Auto-generate toggle → on: mint + show a read-only generated secret; off:
    // clear + enable manual entry + show the weak-password warning.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.autogenerate_toggle.clone().connect_toggled(move |_| {
            apply_autogen_state(&ctx.w);
        });
    }

    // Password show/hide toggle.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .password_show_toggle
            .clone()
            .connect_toggled(move |btn| {
                ctx.w.password_input.set_visibility(btn.is_active());
                btn.set_label(if btn.is_active() { SM::HIDE } else { SM::SHOW });
            });
    }

    // Password strength readout (advisory; never gates submission).
    {
        let strength = ctx.w.password_strength.clone();
        ctx.w.password_input.clone().connect_changed(move |entry| {
            let text =
                fauna_client_mail_settings::password_gen::password_strength_label(&entry.text())
                    .map(|lt| lt.resolve(crate::i18n::strings::lookup))
                    .unwrap_or_default();
            strength.set_text(&text);
        });
    }

    // Submit → EnableMail / AddCredential.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.submit_btn.clone().connect_clicked(move |_| {
            submit_form(&ctx);
        });
    }

    // Cancel → hide the form, re-render (snaps the toggle back if needed).
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.cancel_btn.clone().connect_clicked(move |_| {
            ctx.w.form_group.set_visible(false);
            hydrate_and_render(&ctx);
        });
    }

    // Token copy.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.token_copy_btn.clone().connect_clicked(move |btn| {
            let token = ctx.w.token_display.text().to_string();
            if !token.is_empty() {
                crate::clipboard::copy_text(&token);
                btn.set_label(SM::COPIED);
                let btn_weak = btn.downgrade();
                glib::timeout_add_local_once(Duration::from_secs(2), move || {
                    if let Some(b) = btn_weak.upgrade() {
                        b.set_label(SM::COPY_TOKEN);
                    }
                });
            }
        });
    }

    // Rotate-keys button → open the rotate-keys confirm form.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.rotate_keys_btn.clone().connect_clicked(move |_| {
            open_rotate_form(&ctx);
        });
    }

    // Rotate confirm → StartRotation with the checked (compromised) credentials.
    //
    // The form stays open, its controls disabled and its progress line painted,
    // until the rotation's own dispatch returns (`mail-settings.md` § Element
    // visibility: the progress indicator shows "during multi-step rotation").
    // Closing it on dispatch left nothing on screen while the rotation ran.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.rotate_confirm_btn.clone().connect_clicked(move |_| {
            if ctx.w.rotation_in_flight.get().is_some() {
                return;
            }
            let excluded: Vec<String> = ctx
                .w
                .rotate_exclude_checks
                .borrow()
                .iter()
                .filter(|(_, cb)| cb.is_active())
                .map(|(id, _)| id.clone())
                .collect();
            let snap = ctx.machine.snapshot();
            ctx.w.rotation_in_flight.set(Some(
                fauna_client_mail_settings::state::rotation_rewrap_count(&snap, &excluded),
            ));
            paint_rotate_form(&ctx.w, &snap.status);
            let machine = Arc::clone(&ctx.machine);
            let ctx_render = Rc::clone(&ctx);
            spawn_with_snapshot(
                &ctx.rt,
                move || async move {
                    let _ = machine
                        .dispatch(MailSettingsAction::StartRotation {
                            excluded_credentials: excluded,
                        })
                        .await;
                    machine.snapshot()
                },
                move |snap| {
                    // Only the rotation's own return closes the form: a hydrate
                    // landing mid-rotation re-renders without closing it.
                    ctx_render.w.rotation_in_flight.set(None);
                    ctx_render.w.rotate_form_group.set_visible(false);
                    render(&ctx_render, &snap);
                },
            );
        });
    }

    // Rotate cancel → hide the form, re-render.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.rotate_cancel_btn.clone().connect_clicked(move |_| {
            ctx.w.rotate_form_group.set_visible(false);
            hydrate_and_render(&ctx);
        });
    }

    // Pending-rotation banner: Resume → ResumeRotation.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.resume_btn.clone().connect_clicked(move |_| {
            dispatch_action(&ctx, MailSettingsAction::ResumeRotation);
        });
    }
}

/// Open the rotate-keys confirm form, rebuilding the exclude-list checkboxes
/// from the current snapshot's credentials. Checking a box marks that
/// credential compromised → it is dropped from the rotation (loses access).
fn open_rotate_form(ctx: &Rc<MailPageCtx>) {
    let w = &ctx.w;
    if w.rotation_in_flight.get().is_some() {
        // The form is already open, holding the running rotation's progress.
        return;
    }
    // Rebuild the exclude checkbox list from the current credentials.
    {
        let mut checks = w.rotate_exclude_checks.borrow_mut();
        for (_, cb) in checks.drain(..) {
            w.rotate_exclude_box.remove(&cb);
        }
        for cred in ctx.machine.snapshot().credentials {
            let cb = gtk::CheckButton::with_label(&cred.display_name);
            w.rotate_exclude_box.append(&cb);
            checks.push((cred.credential_id.clone(), cb));
        }
    }
    w.rotate_progress.set_text("");
    super::render_error_label(&w.error_label, None);
    w.rotate_form_group.set_visible(true);
}

/// Paint the rotate form's progress line and controls: the shared rotation
/// label while a rotation runs — this page's own, held open from confirm until
/// its dispatch returns, or one the snapshot reports — with confirm and cancel
/// disabled while this page's own rotation is in flight.
fn paint_rotate_form(w: &MailPageWidgets, status: &SettingsStatus) {
    paint_rotate_controls(
        &w.rotate_progress,
        &w.rotate_confirm_btn,
        &w.rotate_cancel_btn,
        status,
        w.rotation_in_flight.get(),
    );
}

fn paint_rotate_controls(
    progress: &gtk::Label,
    confirm: &gtk::Button,
    cancel: &gtk::Button,
    status: &SettingsStatus,
    in_flight: Option<u64>,
) {
    progress.set_text(&rotate_progress_text(status, in_flight));
    confirm.set_sensitive(in_flight.is_none());
    cancel.set_sensitive(in_flight.is_none());
}

/// The `mail-rotate-keys-progress-indicator` text: the snapshot's own
/// `RotationInProgress` when it reports one; else, while this page's rotation
/// is in flight, the same label painted from the count it re-wraps (the page
/// holds the pre-rotation snapshot until the dispatch returns); else empty.
fn rotate_progress_text(status: &SettingsStatus, in_flight: Option<u64>) -> String {
    let status = match (status, in_flight) {
        (SettingsStatus::RotationInProgress { .. }, _) => status.clone(),
        (_, Some(credentials_remaining)) => SettingsStatus::RotationInProgress {
            credentials_remaining,
        },
        _ => return String::new(),
    };
    fauna_client_mail_settings::settings_status_label(status, true)
        .resolve(crate::i18n::strings::lookup)
}

/// Open the add-credential form in `mode`, resetting it to the input state
/// (OAUTHBEARER default; no token shown yet).
fn open_form(ctx: &Rc<MailPageCtx>, mode: FormMode) {
    ctx.form_mode.set(mode);
    let w = &ctx.w;
    w.name_input.set_text(if mode == FormMode::Enable {
        "Default"
    } else {
        ""
    });
    w.type_selector.set_active(false);
    // Auto-generate defaults ON; the actual fill happens when PLAIN is revealed
    // (the type-selector handler calls apply_autogen_state). Reset the field +
    // warning here for a clean form.
    w.autogenerate_toggle.set_active(true);
    w.password_input.set_text("");
    w.password_input.set_editable(true);
    w.password_show_toggle.set_active(false);
    w.password_strength.set_text("");
    w.weak_password_warning.set_visible(false);
    w.plain_box.set_visible(false);
    w.token_box.set_visible(false);
    w.token_display.set_text("");
    w.form_input_box.set_visible(true);
    w.submit_btn.set_label(if mode == FormMode::Enable {
        SM::SUBMIT_ENABLE
    } else {
        SM::SUBMIT_ADD
    });
    w.cancel_btn.set_label(SM::CANCEL);
    // Clear any stale page error.
    super::render_error_label(&w.error_label, None);
    w.form_group.set_visible(true);
}

/// Read the form, build the action (minting a one-time token for OAUTHBEARER),
/// dispatch it, then render the result — revealing the token on an OAUTHBEARER
/// success, or closing the form on a PLAIN success.
fn submit_form(ctx: &Rc<MailPageCtx>) {
    let w = &ctx.w;
    let mode = ctx.form_mode.get();
    let mut display_name = w.name_input.text().trim().to_string();
    if display_name.is_empty() {
        display_name = "Default".to_string();
    }
    let kind = if w.type_selector.is_active() {
        CredentialKind::Plain
    } else {
        CredentialKind::OAuthBearer
    };

    // Build the secret + (OAUTHBEARER) the human-copyable token string.
    let (secret, token_to_show): (SecretBytes, Option<String>) = match kind {
        CredentialKind::OAuthBearer => {
            let token =
                String::from(fauna_client_mail_settings::password_gen::generate_bridge_token());
            (SecretBytes::from(token.clone().into_bytes()), Some(token))
        }
        CredentialKind::Plain => {
            let pw = w.password_input.text().to_string();
            if pw.is_empty() {
                super::render_error_label(&w.error_label, Some(SM::PASSWORD_REQUIRED));
                return;
            }
            (SecretBytes::from(pw.into_bytes()), None)
        }
    };

    let action = match mode {
        FormMode::Enable => MailSettingsAction::EnableMail {
            display_name,
            kind,
            secret,
        },
        FormMode::Add => MailSettingsAction::AddCredential {
            display_name,
            kind,
            secret,
        },
    };

    // Optimistic in-flight status.
    w.status_marker.set_text(SM::STATUS_SYNCING);
    w.status_row.set_subtitle(SM::STATUS_SYNCING);

    let machine = Arc::clone(&ctx.machine);
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            let _ = machine.dispatch(action).await;
            machine.snapshot()
        },
        move |snap| {
            render(&ctx_render, &snap);
            if snap.error.is_none() {
                match token_to_show {
                    // OAUTHBEARER success: reveal the one-time token; keep the
                    // form open so the user can copy it. Relabel cancel → Done.
                    Some(token) => {
                        ctx_render.w.token_display.set_text(&token);
                        ctx_render.w.form_input_box.set_visible(false);
                        ctx_render.w.token_box.set_visible(true);
                        ctx_render.w.cancel_btn.set_label(SM::DONE);
                    }
                    // PLAIN success: nothing to show once; close the form.
                    None => ctx_render.w.form_group.set_visible(false),
                }
            }
            // On error, render() already surfaced snap.error; leave the form
            // open for retry.
        },
    );
}

/// Run `machine.hydrate()` on the tokio runtime (retrying while the WS socket
/// comes up after login), then render the snapshot on the GTK main thread.
fn hydrate_and_render(ctx: &Rc<MailPageCtx>) {
    let machine = Arc::clone(&ctx.machine);
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            // The WS-RPC connection comes up shortly after login, but the embedded
            // settings page can mount first — the first `config.load` then fails
            // with RpcDisconnected. Retry a few times so the page hydrates as soon
            // as the socket is ready.
            let _ = machine.hydrate().await;
            machine.snapshot()
        },
        move |snap| render(&ctx_render, &snap),
    );
}

/// Render a `MailSettingsSnapshot` into the page widgets. Runs on the GTK main
/// thread. The `guard` suppresses the `toggled` re-fire from setting the toggle
/// programmatically.
fn render(ctx: &Rc<MailPageCtx>, snap: &MailSettingsSnapshot) {
    let w = &ctx.w;

    // The guard suppresses the credential-form re-fire (the SwitchRow's
    // active-notify handler) from this programmatic `set_active` — for both the
    // enabled toggle and the serving toggle.
    ctx.guard.set(true);
    w.enabled_toggle.set_active(snap.enabled);
    w.serve_here_toggle.set_active(snap.serving_enabled);
    ctx.guard.set(false);
    // Carry both toggles' on/off for the e2e driver via the `state` attr (the
    // uniform read idiom: driver.get_attr(id, "state")). The enabled toggle
    // needs its own explicit marker (not just the fallback live-active read
    // below) so the e2e harness's `ensure_mail_enabled` phase-1 gate reads
    // "on"/"off" consistently with apple/windows, instead of the unmarked
    // SwitchRow fallback's "true"/"false".
    set_test_attr(
        &w.enabled_toggle,
        "state",
        if snap.enabled { "on" } else { "off" },
    );
    set_test_attr(
        &w.serve_here_toggle,
        "state",
        if snap.serving_enabled { "on" } else { "off" },
    );

    // The status-indicator decision (Idle-gated-on-enabled / Syncing /
    // RotationInProgress → which `settings.mail.status_*` key) lives in shared Rust
    // `fauna_client_mail_settings::settings_status_label` — the same source of truth
    // web/apple/android consume — so linux delegates instead of hand-rolling the
    // match (which had also hard-coded the English, bypassing i18n).
    // mail-settings.md § the status indicator.
    let status_text =
        fauna_client_mail_settings::settings_status_label(snap.status.clone(), snap.enabled)
            .resolve(crate::i18n::strings::lookup);
    w.status_marker.set_text(&status_text);
    w.status_row.set_subtitle(&status_text);

    // Pending-rotation banner: shown when a previous rotation left a sentinel.
    w.banner_group.set_visible(snap.pending_rotation.is_some());

    paint_rotate_form(w, &snap.status);

    super::render_error_label(&w.error_label, snap.error.as_deref());

    // The shared credential-management section (credentials list, add/rotate,
    // serve-here) renders whenever the actor has a provisioned mailbox — email
    // OR CalDAV OR CardDAV OR serving >=1 folder over WebDAV — so a DAV-only
    // deployment (email off) can still obtain and manage its one shared bridge
    // password (`mail-settings.md` § CalDAV-only mailbox; `caldav-server.md` /
    // `carddav-server.md` § Authentication — the same `(actor, default)`
    // credential AUTHs IMAP + SMTP + CalDAV + CardDAV + WebDAV, so there is
    // nothing protocol-specific to mint, only a previously-hidden affordance to
    // surface). The predicate is computed once in shared Rust (priority #2/#4:
    // `MailSettingsSnapshot::credential_management_reachable`) rather than
    // re-derived per client.
    let mailbox = snap.credential_management_reachable;
    w.credentials_group.set_visible(mailbox);

    // Manage buttons: add visible when a mailbox exists; rotate when ≥1 credential.
    w.manage_group.set_visible(mailbox);
    w.rotate_keys_btn.set_visible(!snap.credentials.is_empty());

    // Serve-here toggle: the per-actor "serve my mailbox over IMAP/CalDAV here"
    // flip applies to a CalDAV-only mailbox just as much as an email one, so it
    // follows `mailbox`, not `enabled`.
    w.serve_here_group.set_visible(mailbox);

    // MUA connection details render whenever a mailbox is provisioned (email OR
    // CalDAV OR CardDAV OR WebDAV). Within the one `mail-settings-mua-instructions`
    // group, the IMAP/SMTP host/port rows describe the *email* protocol → gated on
    // `enabled`; the CalDAV server-URL rows describe the *calendar* protocol →
    // gated on `caldav_enabled` (caldav-server.md § Network exposure —
    // `mail.<domain>:443`, served independent of email); the WebDAV URL row
    // describes the *files* protocol → gated on `serves_webdav_set` (a single
    // full collection-root URL, no host/port split — no SRV autodiscovery exists
    // for WebDAV). Username + AUTH are shared by all (one credential AUTHs
    // IMAP+SMTP+CalDAV+CardDAV+WebDAV), so they show whenever the group does.
    // Values are set unconditionally; per-row visibility decides what the user sees.
    w.mua_group.set_visible(mailbox);
    w.mua_imap_host_row.set_visible(snap.enabled);
    w.mua_imap_port_row.set_visible(snap.enabled);
    w.mua_smtp_host_row.set_visible(snap.enabled);
    w.mua_smtp_port_row.set_visible(snap.enabled);
    w.mua_caldav_host_row.set_visible(snap.caldav_enabled);
    w.mua_caldav_port_row.set_visible(snap.caldav_enabled);
    w.mua_webdav_url_row.set_visible(snap.serves_webdav_set);
    w.mua_imap_host.set_text(&snap.mua.imap_host);
    w.mua_imap_port.set_text(&snap.mua.imap_port.to_string());
    w.mua_smtp_host.set_text(&snap.mua.smtp_host);
    w.mua_smtp_port.set_text(&snap.mua.smtp_port.to_string());
    w.mua_caldav_host.set_text(&snap.mua.caldav_host);
    w.mua_caldav_port
        .set_text(&snap.mua.caldav_port.to_string());
    w.mua_webdav_url.set_text(&snap.mua.webdav_url);
    w.mua_username.set_text(&snap.mua.username_format);
    w.mua_auth.set_text(&snap.mua.auth_mechanism);
}

/// Present the destructive "Disable mail?" confirmation. Confirm dispatches
/// `DisableMail` (bulk-revoke every credential + clear the MSEK, per
/// `mail-settings.md` § Disable mail); cancel is a no-op (the caller already
/// restored the toggle to on). The confirm button carries
/// `mail-settings-disable-confirm-button`; the dialog carries
/// `mail-settings-disable-confirm` for e2e discovery/scoping. Mirrors the
/// established destructive-confirm dialog shape (account delete, folder
/// delete) so the agent drives it via `tag_response_button`.
fn open_disable_confirm(ctx: &Rc<MailPageCtx>) {
    // No wire kind here: the offline gate declares `fauna.account.state.put` on the
    // ENTRY toggle instead (see `build_enabled_row`) — the entry-declares half
    // of the convention `crate::confirm_dialog` documents.
    let anchor = ctx.w.enabled_toggle.clone();
    let ctx = Rc::clone(ctx);
    crate::confirm_dialog::present_confirm(
        &anchor,
        crate::confirm_dialog::ConfirmSpec::new(
            SM::DISABLE_TITLE,
            crate::confirm_dialog::ConfirmBody::Text(SM::DISABLE_WARNING),
            "disable",
            SM::DISABLE_CONFIRM,
            SM::CANCEL,
        )
        .with_dialog_id(ids::MAIL_SETTINGS_DISABLE_CONFIRM)
        .with_confirm_id(ids::MAIL_SETTINGS_DISABLE_CONFIRM_BUTTON),
        move || dispatch_action(&ctx, MailSettingsAction::DisableMail),
    );
}

/// Dispatch a fire-and-render action (revoke; later resume/rotate) on the
/// tokio runtime, then render the resulting snapshot.
fn dispatch_action(ctx: &Rc<MailPageCtx>, action: MailSettingsAction) {
    let machine = Arc::clone(&ctx.machine);
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            let _ = machine.dispatch(action).await;
            machine.snapshot()
        },
        move |snap| render(&ctx_render, &snap),
    );
}

/// Advisory PLAIN-password strength word. Never gates submission.
/// Apply the auto-generate toggle's current state to the PLAIN password field.
///
/// ON (the default): mint a fresh ~143-bit secret via the shared
/// `resolve_autogenerated_password` (the settled-edge mint-once decision — see
/// its doc comment), fill the (masked) field, and make it read-only —
/// shown-once for the user to copy into their MUA (mail-credentials.md
/// § Auto-generated bridge password). OFF: clear it, enable manual entry, and
/// reveal the weak-password warning. Called when the PLAIN fields are revealed
/// (type-selector) and whenever the toggle flips — both settled edges, never
/// at submit, so the field always holds the exact value stored.
fn apply_autogen_state(w: &MailPageWidgets) {
    let resolved = fauna_client_mail_settings::password_gen::resolve_autogenerated_password(
        fauna_client_mail_settings::state::CredentialKind::Plain,
        w.autogenerate_toggle.is_active(),
    );
    if let Some(pw) = resolved {
        w.password_input.set_text(pw.as_str());
        w.password_input.set_editable(false);
        w.password_input.set_visibility(false);
        w.password_show_toggle.set_active(false);
        w.password_strength.set_text("");
        w.weak_password_warning.set_visible(false);
    } else {
        w.password_input.set_text("");
        w.password_input.set_editable(true);
        w.weak_password_warning.set_visible(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testid::find_by_test_id;

    use crate::testid::widget_names;

    /// A confirmed rotation paints its progress line from the moment it is
    /// confirmed, while the page still holds the pre-rotation (`Idle`)
    /// snapshot, and the snapshot's own `RotationInProgress` wins when it has
    /// one; with neither, the line is empty. Before the form held open, the line
    /// was a hard-coded English string no rotation was ever on screen for.
    #[test]
    fn a_rotation_in_flight_paints_the_shared_progress_label() {
        let label = |n: u64| {
            fauna_client_mail_settings::settings_status_label(
                SettingsStatus::RotationInProgress {
                    credentials_remaining: n,
                },
                true,
            )
            .resolve(crate::i18n::strings::lookup)
        };
        assert!(!label(2).is_empty());
        assert_eq!(
            rotate_progress_text(&SettingsStatus::Idle, Some(2)),
            label(2)
        );
        assert_eq!(
            rotate_progress_text(
                &SettingsStatus::RotationInProgress {
                    credentials_remaining: 1
                },
                Some(2)
            ),
            label(1)
        );
        assert_eq!(rotate_progress_text(&SettingsStatus::Idle, None), "");
    }

    /// Confirm holds the rotate form open with both controls disabled while the
    /// rotation runs; its return re-enables them.
    #[test]
    fn a_rotation_in_flight_disables_the_rotate_form_controls() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let confirm = gtk::Button::new();
            let cancel = gtk::Button::new();
            let progress = gtk::Label::new(None);
            let in_flight = Cell::new(Some(3u64));
            paint_rotate_controls(
                &progress,
                &confirm,
                &cancel,
                &SettingsStatus::Idle,
                in_flight.get(),
            );
            assert!(!confirm.is_sensitive() && !cancel.is_sensitive());
            assert!(!progress.text().is_empty());
            in_flight.set(None);
            paint_rotate_controls(
                &progress,
                &confirm,
                &cancel,
                &SettingsStatus::Idle,
                in_flight.get(),
            );
            assert!(confirm.is_sensitive() && cancel.is_sensitive());
            assert_eq!(progress.text(), "");
        });
    }

    /// The Mail page exposes every static ui.yaml ID for the `mail-settings`
    /// page plus the `mail-add-credential` form IDs (Slice 3). The indexed
    /// `mail-settings-credential-item*` rows are added from the snapshot at
    /// render time (no registered client in this test), so they're not asserted
    /// here. The form fields live in the tree regardless of the form group's
    /// initial (hidden) visibility.
    #[test]
    fn mail_page_exposes_static_ui_yaml_ids() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let page = build_mail_page();
            let names = widget_names(&page);
            for id in [
                "page-heading",
                "mail-settings-enabled-toggle",
                "mail-settings-status-indicator",
                "mail-settings-pending-rotation-banner",
                "mail-settings-pending-rotation-resume-button",
                "error-message",
                "mail-settings-add-credential-button",
                "mail-settings-rotate-keys-button",
                "mail-settings-keys-info",
                "mail-settings-serve-here-toggle",
                "mail-settings-mua-instructions",
                "mail-settings-mua-imap-host",
                "mail-settings-mua-imap-port",
                "mail-settings-mua-smtp-host",
                "mail-settings-mua-smtp-port",
                "mail-settings-mua-caldav-host",
                "mail-settings-mua-caldav-port",
                "mail-settings-mua-webdav-url",
                "mail-settings-mua-username-format",
                "mail-settings-mua-auth-mechanism",
                // mail-add-credential form (Slice 3).
                "mail-add-credential-name-input",
                "mail-add-credential-type-selector",
                "mail-add-credential-autogenerate-toggle",
                "mail-add-credential-password-input",
                "mail-add-credential-password-show-toggle",
                "mail-add-credential-password-strength-meter",
                "mail-add-credential-weak-password-warning",
                "mail-add-credential-token-display",
                "mail-add-credential-token-copy-button",
                "mail-add-credential-submit-button",
                "mail-add-credential-cancel-button",
                // mail-rotate-keys-confirm form (Slice 4).
                "mail-rotate-keys-warning-text",
                "mail-rotate-keys-exclude-list",
                "mail-rotate-keys-confirm-button",
                "mail-rotate-keys-cancel-button",
                "mail-rotate-keys-progress-indicator",
            ] {
                assert!(
                    names.iter().any(|n| n == id),
                    "missing widget id {id:?}; have {names:?}",
                );
            }
        });
    }

    /// Regression (startup-log hygiene): the serve-here SwitchRow title is the
    /// plain-text i18n string `SERVE_HERE_LABEL`, which contains a literal `&`
    /// ("Serve my mail & calendar over IMAP/CalDAV on this nest"). `AdwPreferencesRow`
    /// defaults `use-markup=true`, so without disabling markup the bare `&` breaks
    /// Pango parsing (`Gtk-WARNING: Failed to set text … from markup`) and the title
    /// fails to render. The page must disable markup on that row.
    #[test]
    fn serve_here_toggle_disables_markup() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let page = build_mail_page();
            let row = find_by_test_id(&page, "mail-settings-serve-here-toggle")
                .and_then(|w| w.downcast::<adw::SwitchRow>().ok())
                .expect("serve-here SwitchRow present in the mail page");
            assert!(
                !row.uses_markup(),
                "serve-here row must disable Pango markup: i18n strings are plain text \
             and may contain '&'/'<'/'>' which break markup parsing",
            );
        });
    }
}
