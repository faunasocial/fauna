//! The flat **`admin-mail`** policy page (linux; the mail-UX seed's lead app).
//!
//! Where a nest admin tunes the box-wide mail *policy* knobs that have a live
//! write-path: the deployment-wide mail-enable toggle, the Spam/inbound-perimeter
//! sub-struct, and the inbound authentication-enforcement sub-struct. Target
//! behavior + the page §: `docs/goal/behavior/admin.md` § 6 Mail. The policy
//! catalog (which knob exists, its tier/default/binding) is owned by
//! `docs/goal/behavior/mail-policy-config.md` § Policy catalog. UX/IDs:
//! `tests/e2e-unified/ui.yaml` `admin-mail` page.
//!
//! Per `mail-policy-config.md` § UX shell this layer holds **no** business logic
//! — it is a dumb renderer of [`MailPolicySnapshot`] + dispatcher of
//! [`MailPolicyAction`]; the projection (hydrate via `get_mail_config`) + the
//! full-PUT save sequencing live in the shared `fauna_client_mail_settings::
//! admin_policy` machine (priority #2/#4), the prior art the other five apps
//! lift over the `build_mail_policy_machine` UniFFI/wasm export. Direct sibling:
//! `settings/mail_spam.rs` (same wire/hydrate/render glue).
//!
//! # Read + write are both LIVE (not the honest-`unimplemented` user-page pattern)
//!
//! Unlike `mail-spam`/`mail-export` (whose per-user backends are unbuilt), the
//! admin read twin `fauna.bridges.get_mail_config` + the write kinds
//! `set_mail_enabled` / `put_{spam,auth}_policy` all exist end-to-end. nest
//! rejects an out-of-order spam-threshold write (`fauna.protocol.malformed`);
//! the machine surfaces it via `MailPolicySnapshot::error`, never faked green.
//!
//! # Discoverability
//!
//! Every actuable control (`gtk::Switch`, `gtk::Entry`, `gtk::DropDown`,
//! `gtk::Button`, the dnsbl `gtk::TextView`) carries its ui.yaml ID on the widget
//! itself (the in-process agent reads/sets them directly). The page-heading is a
//! 1px marker on the top group's header (adw rows aren't AT-SPI-readable). The
//! page is registered as an admin-shell `gtk::Stack` sub-page (child `admin-mail`,
//! mirroring `admin-services`), reached through the state protocol.

use fauna_ui_ids as ids;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;

use fauna_client_mail_settings::{
    AliasPolicyView, AuthPolicyView, FcrdnsMode, ImapDeleteNonempty, ImapPolicyView,
    MailPolicyAction, MailPolicyMachine, MailPolicySnapshot, OutboundPolicyView, SpamPolicyView,
    SubmissionPolicyView,
};

use crate::async_helper::spawn_with_snapshot;
use crate::i18n::strings::admin::mail_page as S;
use crate::testid::set_test_id;

// The page's two raw-value picker vocabularies — their DropDown order, their
// wire tokens, and the unknown-token fallback each hydrate applies — are owned
// by `fauna_client_mail_settings::admin_policy::{FcrdnsMode, ImapDeleteNonempty}`.
// This file held a byte-identical copy of both arrays plus its own two
// index-lookup helpers until 2026-08-23, tui held the same two arrays, and Go's
// `ParseFCrDNSMode` a third copy of the FCrDNS one — including its fallback
// rule, which is a safety decision rather than a rendering detail
// (priority #1/#2/#4).

/// Handles to the widgets the snapshot renders into / the save reads from. GTK
/// objects are reference-counted, so cloning this is cheap and shares widgets.
#[derive(Clone)]
struct MailWidgets {
    error_label: gtk::Label,
    enabled_toggle: gtk::Switch,
    auto_enable_new_users: gtk::Switch,

    // ── Spam / inbound perimeter (put_spam_policy) ──
    threshold_junk: gtk::Entry,
    threshold_reject: gtk::Entry,
    dnsbl_servers: gtk::TextView,
    reject_no_rdns: gtk::Switch,
    greylist_enabled: gtk::Switch,
    greylist_delay: gtk::Entry,
    max_conn_per_min: gtk::Entry,
    fcrdns_mode: gtk::DropDown,
    helo_identity_required: gtk::Switch,
    reject_fcrdns_fail: gtk::Switch,
    max_message_bytes: gtk::Entry,
    // Per-user training (Tier-2 combined-score knobs; same put_spam_policy).
    bayesian_weight: gtk::Entry,
    bayesian_min_samples: gtk::Entry,
    bayesian_full_confidence_samples: gtk::Entry,
    training_history_retention: gtk::Entry,
    // Recipient-whitelist penalty (deployment-wide; same put_spam_policy).
    unlisted_recipient_penalty: gtk::Entry,
    // Deployment baseline (admin opt-in aggregate; publish_spam_baseline). The
    // button lives in `MailSaveButtons`; this is only the render target for the
    // published/withheld outcome.
    publish_baseline_result: gtk::Label,

    // ── Inbound authentication enforcement (put_auth_policy) ──
    enforce_dmarc: gtk::Switch,
    enforce_dmarc_quarantine: gtk::Switch,
    enforce_spf_hardfail: gtk::Switch,
    enforce_dkim: gtk::Switch,
    log_only: gtk::Switch,
    max_failures: gtk::Entry,
    max_conn_per_ip: gtk::Entry,

    // ── Submission quotas (put_submission_policy) ──
    submission_max_per_day: gtk::Entry,
    submission_max_recipients: gtk::Entry,

    // ── IMAP server policy (put_imap_policy) ──
    imap_idle_timeout: gtk::Entry,
    imap_tombstone_retention: gtk::Entry,
    imap_delete_nonempty: gtk::DropDown,
    imap_bodystructure_cache: gtk::Entry,
    imap_storage_bytes: gtk::Entry,
    imap_message_count: gtk::Entry,

    // ── Outbound delivery (put_outbound_policy) ──
    outbound_retry_schedule: gtk::TextView,
    outbound_permfail_timeout: gtk::Entry,
    outbound_delay_warning: gtk::Entry,
    outbound_ndr_rate_limit: gtk::Entry,
    outbound_suppress_ndr_spf: gtk::Switch,
    outbound_suppress_ndr_dmarc: gtk::Switch,
    outbound_postmaster_cc: gtk::Switch,
    outbound_tlsrpt_send: gtk::Switch,
    outbound_ipv6: gtk::Switch,
    outbound_treat_5xx_transient: gtk::TextView,

    // ── Aliases (put_alias_policy — nest-side, separate get_alias_policy twin) ──
    alias_exact_max: gtk::Entry,
    alias_reserved_local_parts: gtk::TextView,
    alias_subaddressing: gtk::Switch,
    alias_wildcard_prefix: gtk::Switch,
}

/// Everything the page's handlers + render need. `Rc`-shared into every closure.
struct MailCtx {
    machine: Arc<MailPolicyMachine>,
    rt: tokio::runtime::Handle,
    /// Set while `render()` programmatically updates a control whose change
    /// handler dispatches (the mail-enable toggle), so its handler doesn't echo
    /// the change back as an action. The Spam/Auth controls are read on Save
    /// (no per-change dispatch), so they need no guard.
    syncing: Cell<bool>,
    w: MailWidgets,
}

/// Add a raw-integer `gtk::Entry` row (title + subtitle) to `group`, ID on the
/// entry. The entry parses as `u32` on save with a fallback to the persisted
/// value (so a stray/blank edit never silently zeroes a knob; mirrors
/// `views/admin.rs::parse_cap`).
fn entry_row(
    group: &adw::PreferencesGroup,
    title: &str,
    subtitle: &str,
    test_id: &str,
) -> gtk::Entry {
    let entry = gtk::Entry::builder()
        .valign(gtk::Align::Center)
        .width_chars(12)
        .build();
    set_test_id(&entry, test_id);
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(subtitle)
        .activatable(false)
        .build();
    row.add_suffix(&entry);
    group.add(&row);
    entry
}

/// Add a captioned multiline `gtk::TextView` block (one value per line) to
/// `group`, ID on the TextView. Used for the list-valued knobs (dnsbl servers,
/// outbound retry schedule, transient-5xx codes).
fn textview_block(
    group: &adw::PreferencesGroup,
    caption: &str,
    subtitle: &str,
    test_id: &str,
) -> gtk::TextView {
    let view = gtk::TextView::builder()
        .height_request(64)
        .accepts_tab(false)
        .build();
    set_test_id(&view, test_id);
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .min_content_height(64)
        .child(&view)
        .build();
    scroll.add_css_class("card");
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 4);
    outer.set_margin_top(4);
    outer.set_margin_bottom(4);
    let caption_label = gtk::Label::builder()
        .label(caption)
        .halign(gtk::Align::Start)
        .css_classes(["heading"])
        .build();
    let subtitle_label = gtk::Label::builder()
        .label(subtitle)
        .halign(gtk::Align::Start)
        .css_classes(["dim-label", "caption"])
        .build();
    outer.append(&caption_label);
    outer.append(&subtitle_label);
    outer.append(&scroll);
    group.add(&outer);
    view
}

/// Add a `gtk::DropDown` row (title + the option strings) to `group`, ID on the
/// dropdown. The caller maps the selected index ↔ wire string.
fn dropdown_row(
    group: &adw::PreferencesGroup,
    title: &str,
    options: &[&str],
    test_id: &str,
) -> gtk::DropDown {
    let dropdown = gtk::DropDown::from_strings(options);
    dropdown.set_valign(gtk::Align::Center);
    set_test_id(&dropdown, test_id);
    let row = adw::ActionRow::builder()
        .title(title)
        .activatable(false)
        .build();
    row.add_suffix(&dropdown);
    group.add(&row);
    dropdown
}

/// Build the flat `admin-mail` policy page.
pub fn build_admin_mail_page() -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title(S::TITLE)
        .icon_name("mail-message-new-symbolic")
        .build();

    // --- Top group: heading + page-level error + the mail-enable master toggle.
    let top_group = adw::PreferencesGroup::builder()
        .title(S::TITLE)
        .description(S::DESCRIPTION)
        .build();
    top_group.set_header_suffix(Some(&super::marker("page-heading")));

    // error-message — page-level error label (Rule 2), hidden until set.
    let error_label = gtk::Label::builder().visible(false).build();
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    let error_row = adw::ActionRow::builder().activatable(false).build();
    error_row.add_suffix(&error_label);
    top_group.add(&error_row);

    // admin-mail-enabled-toggle — mail.enabled (set_mail_enabled); dispatches
    // immediately on change (re-reads persisted state).
    let enabled_toggle = super::switch_row(
        &top_group,
        S::ENABLED_LABEL,
        S::ENABLED_SUBTITLE,
        "admin-mail-enabled-toggle",
    );
    // tui's `admin::Action::ToggleMailEnabled`.
    crate::offline_gate::declare_wire_kind(&enabled_toggle, "fauna.bridges.set_mail_enabled");
    // admin-mail-auto-enable-new-users-toggle — auto_enable_mail_for_new_users
    // (set_auto_enable_mail_for_new_users; read back via fauna.setup.status).
    // Default-on deployment policy; dispatches immediately on change.
    let auto_enable_new_users = super::switch_row(
        &top_group,
        S::AUTO_ENABLE_NEW_USERS_LABEL,
        S::AUTO_ENABLE_NEW_USERS_SUBTITLE,
        "admin-mail-auto-enable-new-users-toggle",
    );
    // tui's `admin::Action::ToggleMailAutoEnable`.
    crate::offline_gate::declare_wire_kind(
        &auto_enable_new_users,
        "fauna.bridges.set_auto_enable_mail_for_new_users",
    );
    page.add(&top_group);

    // --- Spam / inbound perimeter group (put_spam_policy) ---
    let spam_group = adw::PreferencesGroup::builder()
        .title(S::SPAM_GROUP_TITLE)
        .description(S::SPAM_GROUP_DESC)
        .build();

    let threshold_junk = entry_row(
        &spam_group,
        S::THRESHOLD_JUNK_LABEL,
        S::THRESHOLD_JUNK_SUBTITLE,
        "admin-mail-spam-threshold-junk",
    );
    let threshold_reject = entry_row(
        &spam_group,
        S::THRESHOLD_REJECT_LABEL,
        S::THRESHOLD_REJECT_SUBTITLE,
        "admin-mail-spam-threshold-reject",
    );

    // admin-mail-dnsbl-servers — multiline (one host per line); ID on the TextView.
    let dnsbl_servers = textview_block(
        &spam_group,
        S::DNSBL_LABEL,
        S::DNSBL_SUBTITLE,
        "admin-mail-dnsbl-servers",
    );

    let reject_no_rdns = super::switch_row(
        &spam_group,
        S::REJECT_NO_RDNS_LABEL,
        "",
        "admin-mail-reject-no-rdns-toggle",
    );
    let greylist_enabled = super::switch_row(
        &spam_group,
        S::GREYLIST_ENABLED_LABEL,
        "",
        "admin-mail-greylist-enabled-toggle",
    );
    let greylist_delay = entry_row(
        &spam_group,
        S::GREYLIST_DELAY_LABEL,
        "",
        "admin-mail-greylist-delay-input",
    );
    let max_conn_per_min = entry_row(
        &spam_group,
        S::MAX_CONN_PER_MIN_LABEL,
        "",
        "admin-mail-max-conn-per-min-input",
    );

    // admin-mail-fcrdns-mode-select — off / score_signal / enforce; value-via-state.
    let fcrdns_mode = dropdown_row(
        &spam_group,
        S::FCRDNS_MODE_LABEL,
        &[S::FCRDNS_OFF, S::FCRDNS_SCORE_SIGNAL, S::FCRDNS_ENFORCE],
        "admin-mail-fcrdns-mode-select",
    );

    let helo_identity_required = super::switch_row(
        &spam_group,
        S::HELO_IDENTITY_LABEL,
        "",
        "admin-mail-helo-identity-required-toggle",
    );
    let reject_fcrdns_fail = super::switch_row(
        &spam_group,
        S::REJECT_FCRDNS_FAIL_LABEL,
        "",
        "admin-mail-reject-fcrdns-fail-toggle",
    );
    let max_message_bytes = entry_row(
        &spam_group,
        S::MAX_MESSAGE_BYTES_LABEL,
        "",
        "admin-mail-max-message-bytes-input",
    );

    // ── Per-user training (Tier-2 combined-score knobs; same put_spam_policy) ──
    let bayesian_weight = entry_row(
        &spam_group,
        S::BAYESIAN_WEIGHT_LABEL,
        S::BAYESIAN_WEIGHT_SUBTITLE,
        "admin-mail-spam-bayesian-weight",
    );
    let bayesian_min_samples = entry_row(
        &spam_group,
        S::BAYESIAN_MIN_SAMPLES_LABEL,
        S::BAYESIAN_MIN_SAMPLES_SUBTITLE,
        "admin-mail-spam-bayesian-min-samples",
    );
    let bayesian_full_confidence_samples = entry_row(
        &spam_group,
        S::BAYESIAN_FULL_CONFIDENCE_SAMPLES_LABEL,
        S::BAYESIAN_FULL_CONFIDENCE_SAMPLES_SUBTITLE,
        "admin-mail-spam-bayesian-full-confidence-samples",
    );
    let training_history_retention = entry_row(
        &spam_group,
        S::TRAINING_HISTORY_RETENTION_LABEL,
        S::TRAINING_HISTORY_RETENTION_SUBTITLE,
        "admin-mail-spam-training-history-retention",
    );
    // ── Recipient-whitelist unlisted-recipient penalty (points; 0 = off) ──
    // Note the test ID has no `spam-` segment (ui.yaml § admin-mail), unlike its
    // siblings above; it is still part of the same put_spam_policy full-PUT.
    let unlisted_recipient_penalty = entry_row(
        &spam_group,
        S::UNLISTED_RECIPIENT_PENALTY_LABEL,
        S::UNLISTED_RECIPIENT_PENALTY_SUBTITLE,
        "admin-mail-unlisted-recipient-penalty",
    );

    // admin-mail-spam-save-button — full PUT of the whole spam sub-struct.
    let spam_save = gtk::Button::builder()
        .label(S::SPAM_SAVE)
        .valign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .build();
    set_test_id(&spam_save, ids::ADMIN_MAIL_SPAM_SAVE_BUTTON);
    // tui's `admin::Action::SaveMailSpam`.
    crate::offline_gate::declare_wire_kind(&spam_save, "fauna.bridges.put_spam_policy");
    let spam_save_row = adw::ActionRow::builder().activatable(false).build();
    spam_save_row.add_suffix(&spam_save);
    spam_group.add(&spam_save_row);

    // admin-mail-publish-spam-baseline-button — publish the current aggregate of
    // opt-in users' models as the deployment baseline (fauna.bridges.
    // publish_spam_baseline). The result row below shows the published/withheld
    // outcome; nest's k-anonymity floor withholds below 3 opt-in contributors.
    let publish_baseline = gtk::Button::builder()
        .label(S::PUBLISH_SPAM_BASELINE_BUTTON)
        .valign(gtk::Align::Center)
        .build();
    set_test_id(
        &publish_baseline,
        ids::ADMIN_MAIL_PUBLISH_SPAM_BASELINE_BUTTON,
    );
    // tui's `admin::Action::PublishSpamBaseline`.
    crate::offline_gate::declare_wire_kind(
        &publish_baseline,
        "fauna.bridges.publish_spam_baseline",
    );
    let publish_baseline_row = adw::ActionRow::builder()
        .subtitle(S::PUBLISH_SPAM_BASELINE_SUBTITLE)
        .activatable(false)
        .build();
    publish_baseline_row.add_suffix(&publish_baseline);
    spam_group.add(&publish_baseline_row);
    // admin-mail-publish-spam-baseline-result — last publish outcome, EMPTY (not
    // hidden) until the admin clicks Publish (rendered from
    // snapshot.baseline_publish_result). It is a required `elements` entry for
    // this page in ui.yaml, and a GTK widget built `.visible(false)` is absent
    // from the a11y tree entirely — so hiding it made linux the one app that does
    // not render a required element, and `test_publish_spam_baseline_controls_
    // render[linux]` red on main while tui and web passed. Its sibling
    // `..._withheld[linux]` passed throughout precisely because it clicks Publish
    // first, which un-hid the label before reading it. The wrapping ActionRow is
    // unconditionally visible either way, so an always-present empty label is
    // visually identical to the old hidden one.
    let publish_baseline_result = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .wrap(true)
        .css_classes(["dim-label", "caption"])
        .build();
    set_test_id(
        &publish_baseline_result,
        ids::ADMIN_MAIL_PUBLISH_SPAM_BASELINE_RESULT,
    );
    let publish_baseline_result_row = adw::ActionRow::builder().activatable(false).build();
    publish_baseline_result_row.add_suffix(&publish_baseline_result);
    spam_group.add(&publish_baseline_result_row);
    page.add(&spam_group);

    // --- Inbound authentication enforcement group (put_auth_policy) ---
    let auth_group = adw::PreferencesGroup::builder()
        .title(S::AUTH_GROUP_TITLE)
        .description(S::AUTH_GROUP_DESC)
        .build();

    let enforce_dmarc = super::switch_row(
        &auth_group,
        S::ENFORCE_DMARC_LABEL,
        "",
        "admin-mail-auth-enforce-dmarc-toggle",
    );
    let enforce_dmarc_quarantine = super::switch_row(
        &auth_group,
        S::ENFORCE_DMARC_QUARANTINE_LABEL,
        "",
        "admin-mail-auth-enforce-dmarc-quarantine-toggle",
    );
    let enforce_spf_hardfail = super::switch_row(
        &auth_group,
        S::ENFORCE_SPF_HARDFAIL_LABEL,
        "",
        "admin-mail-auth-enforce-spf-hardfail-toggle",
    );
    let enforce_dkim = super::switch_row(
        &auth_group,
        S::ENFORCE_DKIM_LABEL,
        "",
        "admin-mail-auth-enforce-dkim-toggle",
    );
    let log_only = super::switch_row(
        &auth_group,
        S::LOG_ONLY_LABEL,
        "",
        "admin-mail-auth-log-only-toggle",
    );
    let max_failures = entry_row(
        &auth_group,
        S::MAX_FAILURES_LABEL,
        "",
        "admin-mail-auth-max-failures-input",
    );
    let max_conn_per_ip = entry_row(
        &auth_group,
        S::MAX_CONN_PER_IP_LABEL,
        "",
        "admin-mail-auth-max-conn-per-ip-input",
    );

    // admin-mail-auth-save-button — full PUT of the whole auth sub-struct.
    let auth_save = gtk::Button::builder()
        .label(S::AUTH_SAVE)
        .valign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .build();
    set_test_id(&auth_save, ids::ADMIN_MAIL_AUTH_SAVE_BUTTON);
    // tui's `admin::Action::SaveMailAuth`.
    crate::offline_gate::declare_wire_kind(&auth_save, "fauna.bridges.put_auth_policy");
    let auth_save_row = adw::ActionRow::builder().activatable(false).build();
    auth_save_row.add_suffix(&auth_save);
    auth_group.add(&auth_save_row);
    page.add(&auth_group);

    // --- Submission quotas group (put_submission_policy) ---
    let submission_group = adw::PreferencesGroup::builder()
        .title(S::SUBMISSION_GROUP_TITLE)
        .description(S::SUBMISSION_GROUP_DESC)
        .build();
    let submission_max_per_day = entry_row(
        &submission_group,
        S::SUBMISSION_MAX_PER_DAY_LABEL,
        S::SUBMISSION_MAX_PER_DAY_SUBTITLE,
        "admin-mail-submission-max-per-day-input",
    );
    let submission_max_recipients = entry_row(
        &submission_group,
        S::SUBMISSION_MAX_RECIPIENTS_LABEL,
        S::SUBMISSION_MAX_RECIPIENTS_SUBTITLE,
        "admin-mail-submission-max-recipients-input",
    );
    let submission_save = gtk::Button::builder()
        .label(S::SUBMISSION_SAVE)
        .valign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .build();
    set_test_id(&submission_save, ids::ADMIN_MAIL_SUBMISSION_SAVE_BUTTON);
    // tui's `admin::Action::SaveMailSubmission`.
    crate::offline_gate::declare_wire_kind(&submission_save, "fauna.bridges.put_submission_policy");
    let submission_save_row = adw::ActionRow::builder().activatable(false).build();
    submission_save_row.add_suffix(&submission_save);
    submission_group.add(&submission_save_row);
    page.add(&submission_group);

    // --- IMAP server policy group (put_imap_policy) ---
    let imap_group = adw::PreferencesGroup::builder()
        .title(S::IMAP_GROUP_TITLE)
        .description(S::IMAP_GROUP_DESC)
        .build();
    let imap_idle_timeout = entry_row(
        &imap_group,
        S::IMAP_IDLE_TIMEOUT_LABEL,
        S::IMAP_IDLE_TIMEOUT_SUBTITLE,
        "admin-mail-imap-idle-timeout-input",
    );
    let imap_tombstone_retention = entry_row(
        &imap_group,
        S::IMAP_TOMBSTONE_RETENTION_LABEL,
        S::IMAP_TOMBSTONE_RETENTION_SUBTITLE,
        "admin-mail-imap-tombstone-retention-input",
    );
    let imap_delete_nonempty = dropdown_row(
        &imap_group,
        S::IMAP_DELETE_NONEMPTY_LABEL,
        &[S::IMAP_DELETE_FORBIDDEN, S::IMAP_DELETE_ALLOWED],
        "admin-mail-imap-delete-nonempty-select",
    );
    let imap_bodystructure_cache = entry_row(
        &imap_group,
        S::IMAP_BODYSTRUCTURE_CACHE_LABEL,
        S::IMAP_BODYSTRUCTURE_CACHE_SUBTITLE,
        "admin-mail-imap-bodystructure-cache-input",
    );
    let imap_storage_bytes = entry_row(
        &imap_group,
        S::IMAP_STORAGE_BYTES_LABEL,
        S::IMAP_STORAGE_BYTES_SUBTITLE,
        "admin-mail-imap-storage-bytes-input",
    );
    let imap_message_count = entry_row(
        &imap_group,
        S::IMAP_MESSAGE_COUNT_LABEL,
        S::IMAP_MESSAGE_COUNT_SUBTITLE,
        "admin-mail-imap-message-count-input",
    );
    let imap_save = gtk::Button::builder()
        .label(S::IMAP_SAVE)
        .valign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .build();
    set_test_id(&imap_save, ids::ADMIN_MAIL_IMAP_SAVE_BUTTON);
    // tui's `admin::Action::SaveMailImap`.
    crate::offline_gate::declare_wire_kind(&imap_save, "fauna.bridges.put_imap_policy");
    let imap_save_row = adw::ActionRow::builder().activatable(false).build();
    imap_save_row.add_suffix(&imap_save);
    imap_group.add(&imap_save_row);
    page.add(&imap_group);

    // --- Outbound delivery group (put_outbound_policy) ---
    let outbound_group = adw::PreferencesGroup::builder()
        .title(S::OUTBOUND_GROUP_TITLE)
        .description(S::OUTBOUND_GROUP_DESC)
        .build();
    // admin-mail-outbound-retry-schedule — multiline (one delay-seconds value per line).
    let outbound_retry_schedule = textview_block(
        &outbound_group,
        S::OUTBOUND_RETRY_SCHEDULE_LABEL,
        S::OUTBOUND_RETRY_SCHEDULE_SUBTITLE,
        "admin-mail-outbound-retry-schedule",
    );
    let outbound_permfail_timeout = entry_row(
        &outbound_group,
        S::OUTBOUND_PERMFAIL_TIMEOUT_LABEL,
        S::OUTBOUND_PERMFAIL_TIMEOUT_SUBTITLE,
        "admin-mail-outbound-permfail-timeout-input",
    );
    let outbound_delay_warning = entry_row(
        &outbound_group,
        S::OUTBOUND_DELAY_WARNING_LABEL,
        S::OUTBOUND_DELAY_WARNING_SUBTITLE,
        "admin-mail-outbound-delay-warning-input",
    );
    let outbound_ndr_rate_limit = entry_row(
        &outbound_group,
        S::OUTBOUND_NDR_RATE_LIMIT_LABEL,
        S::OUTBOUND_NDR_RATE_LIMIT_SUBTITLE,
        "admin-mail-outbound-ndr-rate-limit-input",
    );
    let outbound_suppress_ndr_spf = super::switch_row(
        &outbound_group,
        S::OUTBOUND_SUPPRESS_NDR_SPF_LABEL,
        "",
        "admin-mail-outbound-suppress-ndr-spf-toggle",
    );
    let outbound_suppress_ndr_dmarc = super::switch_row(
        &outbound_group,
        S::OUTBOUND_SUPPRESS_NDR_DMARC_LABEL,
        "",
        "admin-mail-outbound-suppress-ndr-dmarc-toggle",
    );
    // postmaster CC — project policy is never CC; rendered read-only (disabled in v1).
    let outbound_postmaster_cc = super::switch_row(
        &outbound_group,
        S::OUTBOUND_POSTMASTER_CC_LABEL,
        S::OUTBOUND_POSTMASTER_CC_SUBTITLE,
        "admin-mail-outbound-postmaster-cc-toggle",
    );
    outbound_postmaster_cc.set_sensitive(false);
    let outbound_tlsrpt_send = super::switch_row(
        &outbound_group,
        S::OUTBOUND_TLSRPT_SEND_LABEL,
        "",
        "admin-mail-outbound-tlsrpt-send-toggle",
    );
    let outbound_ipv6 = super::switch_row(
        &outbound_group,
        S::OUTBOUND_IPV6_LABEL,
        "",
        "admin-mail-outbound-ipv6-toggle",
    );
    // admin-mail-outbound-treat-5xx-transient — multiline (one enhanced code per line).
    let outbound_treat_5xx_transient = textview_block(
        &outbound_group,
        S::OUTBOUND_TREAT_5XX_LABEL,
        S::OUTBOUND_TREAT_5XX_SUBTITLE,
        "admin-mail-outbound-treat-5xx-transient",
    );
    let outbound_save = gtk::Button::builder()
        .label(S::OUTBOUND_SAVE)
        .valign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .build();
    set_test_id(&outbound_save, ids::ADMIN_MAIL_OUTBOUND_SAVE_BUTTON);
    // tui's `admin::Action::SaveMailOutbound`.
    crate::offline_gate::declare_wire_kind(&outbound_save, "fauna.bridges.put_outbound_policy");
    let outbound_save_row = adw::ActionRow::builder().activatable(false).build();
    outbound_save_row.add_suffix(&outbound_save);
    outbound_group.add(&outbound_save_row);
    page.add(&outbound_group);

    // --- Aliases group (put_alias_policy; nest-side, separate read twin) ---
    // Unlike the five groups above, this group hydrates from `get_alias_policy`
    // (the alias knobs are not in `FetchConfigReply`) — but renders identically.
    let alias_group = adw::PreferencesGroup::builder()
        .title(S::ALIAS_GROUP_TITLE)
        .description(S::ALIAS_GROUP_DESC)
        .build();
    let alias_exact_max = entry_row(
        &alias_group,
        S::ALIAS_EXACT_MAX_LABEL,
        S::ALIAS_EXACT_MAX_SUBTITLE,
        "admin-mail-alias-exact-max-input",
    );
    // admin-mail-alias-reserved-local-parts — multiline (one local-part per
    // line). An empty box is a meaningful override (clears the reservation),
    // matching `PutAliasPolicyRequest`'s `Some(vec![])` semantics.
    let alias_reserved_local_parts = textview_block(
        &alias_group,
        S::ALIAS_RESERVED_LABEL,
        S::ALIAS_RESERVED_SUBTITLE,
        "admin-mail-alias-reserved-local-parts",
    );
    let alias_subaddressing = super::switch_row(
        &alias_group,
        S::ALIAS_SUBADDRESSING_LABEL,
        S::ALIAS_SUBADDRESSING_SUBTITLE,
        "admin-mail-alias-subaddressing-toggle",
    );
    let alias_wildcard_prefix = super::switch_row(
        &alias_group,
        S::ALIAS_WILDCARD_PREFIX_LABEL,
        S::ALIAS_WILDCARD_PREFIX_SUBTITLE,
        "admin-mail-alias-wildcard-prefix-toggle",
    );
    let alias_save = gtk::Button::builder()
        .label(S::ALIAS_SAVE)
        .valign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .build();
    set_test_id(&alias_save, ids::ADMIN_MAIL_ALIAS_SAVE_BUTTON);
    // tui's `admin::Action::SaveMailAlias`.
    crate::offline_gate::declare_wire_kind(&alias_save, "fauna.bridges.put_alias_policy");
    let alias_save_row = adw::ActionRow::builder().activatable(false).build();
    alias_save_row.add_suffix(&alias_save);
    alias_group.add(&alias_save_row);
    page.add(&alias_group);

    let widgets = MailWidgets {
        error_label,
        enabled_toggle,
        auto_enable_new_users,
        threshold_junk,
        threshold_reject,
        dnsbl_servers,
        reject_no_rdns,
        greylist_enabled,
        greylist_delay,
        max_conn_per_min,
        fcrdns_mode,
        helo_identity_required,
        reject_fcrdns_fail,
        max_message_bytes,
        bayesian_weight,
        bayesian_min_samples,
        bayesian_full_confidence_samples,
        training_history_retention,
        unlisted_recipient_penalty,
        publish_baseline_result,
        enforce_dmarc,
        enforce_dmarc_quarantine,
        enforce_spf_hardfail,
        enforce_dkim,
        log_only,
        max_failures,
        max_conn_per_ip,
        submission_max_per_day,
        submission_max_recipients,
        imap_idle_timeout,
        imap_tombstone_retention,
        imap_delete_nonempty,
        imap_bodystructure_cache,
        imap_storage_bytes,
        imap_message_count,
        outbound_retry_schedule,
        outbound_permfail_timeout,
        outbound_delay_warning,
        outbound_ndr_rate_limit,
        outbound_suppress_ndr_spf,
        outbound_suppress_ndr_dmarc,
        outbound_postmaster_cc,
        outbound_tlsrpt_send,
        outbound_ipv6,
        outbound_treat_5xx_transient,
        alias_exact_max,
        alias_reserved_local_parts,
        alias_subaddressing,
        alias_wildcard_prefix,
    };
    let saves = MailSaveButtons {
        spam: spam_save,
        auth: auth_save,
        submission: submission_save,
        imap: imap_save,
        outbound: outbound_save,
        alias: alias_save,
        publish_baseline,
    };
    wire_machine(saves, widgets);

    page
}

/// The per-group Save buttons, threaded to `wire_machine` so each can gather +
/// dispatch its own full-PUT.
struct MailSaveButtons {
    spam: gtk::Button,
    auth: gtk::Button,
    submission: gtk::Button,
    imap: gtk::Button,
    outbound: gtk::Button,
    alias: gtk::Button,
    /// Not a full-PUT save — dispatches `PublishSpamBaseline` (no gather).
    publish_baseline: gtk::Button,
}

/// Connect the page to the shared `MailPolicyMachine`, hydrate on mount, and wire
/// every interaction. No-op (page stays at static placeholders) when no client
/// is available — e.g. the unit test, which has no registered client.
fn wire_machine(saves: MailSaveButtons, widgets: MailWidgets) {
    let client = match crate::settings::get_client() {
        Some(c) => c,
        None => return,
    };
    let machine = Arc::new(crate::mail_glue::build_mail_policy_machine(&client));

    let ctx = Rc::new(MailCtx {
        machine,
        rt: client.runtime_handle(),
        syncing: Cell::new(false),
        w: widgets,
    });

    hydrate_and_render(&ctx);

    // Mail-enable toggle → SetMailEnabled (skip the echo from render()).
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .enabled_toggle
            .clone()
            .connect_active_notify(move |sw| {
                if ctx.syncing.get() {
                    return;
                }
                dispatch_action(
                    &ctx,
                    MailPolicyAction::SetMailEnabled {
                        enabled: sw.is_active(),
                    },
                );
            });
    }

    // Auto-enable-for-new-users toggle → SetAutoEnableMailForNewUsers (skip the
    // echo from render()).
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .auto_enable_new_users
            .clone()
            .connect_active_notify(move |sw| {
                if ctx.syncing.get() {
                    return;
                }
                dispatch_action(
                    &ctx,
                    MailPolicyAction::SetAutoEnableMailForNewUsers {
                        enabled: sw.is_active(),
                    },
                );
            });
    }

    // Spam group Save → gather the whole sub-struct → SaveSpam (full PUT).
    {
        let ctx = Rc::clone(&ctx);
        saves.spam.connect_clicked(move |_| {
            let policy = gather_spam(&ctx);
            dispatch_action(&ctx, MailPolicyAction::SaveSpam { policy });
        });
    }

    // Auth group Save → gather the whole sub-struct → SaveAuth (full PUT).
    {
        let ctx = Rc::clone(&ctx);
        saves.auth.connect_clicked(move |_| {
            let policy = gather_auth(&ctx);
            dispatch_action(&ctx, MailPolicyAction::SaveAuth { policy });
        });
    }

    // Submission group Save → gather the whole sub-struct → SaveSubmission.
    {
        let ctx = Rc::clone(&ctx);
        saves.submission.connect_clicked(move |_| {
            let policy = gather_submission(&ctx);
            dispatch_action(&ctx, MailPolicyAction::SaveSubmission { policy });
        });
    }

    // IMAP group Save → gather the whole sub-struct → SaveImap.
    {
        let ctx = Rc::clone(&ctx);
        saves.imap.connect_clicked(move |_| {
            let policy = gather_imap(&ctx);
            dispatch_action(&ctx, MailPolicyAction::SaveImap { policy });
        });
    }

    // Outbound group Save → gather the whole sub-struct → SaveOutbound.
    {
        let ctx = Rc::clone(&ctx);
        saves.outbound.connect_clicked(move |_| {
            let policy = gather_outbound(&ctx);
            dispatch_action(&ctx, MailPolicyAction::SaveOutbound { policy });
        });
    }

    // Aliases group Save → gather the whole sub-struct → SaveAlias.
    {
        let ctx = Rc::clone(&ctx);
        saves.alias.connect_clicked(move |_| {
            let policy = gather_alias(&ctx);
            dispatch_action(&ctx, MailPolicyAction::SaveAlias { policy });
        });
    }

    // Publish deployment baseline → PublishSpamBaseline (no gather; the outcome
    // lands in snapshot.baseline_publish_result, rendered on the result label).
    {
        let ctx = Rc::clone(&ctx);
        saves.publish_baseline.connect_clicked(move |_| {
            dispatch_action(&ctx, MailPolicyAction::PublishSpamBaseline);
        });
    }
}

/// Run `machine.hydrate()` on the tokio runtime (retrying while the WS socket
/// comes up after login), then render the snapshot on the GTK main thread.
fn hydrate_and_render(ctx: &Rc<MailCtx>) {
    let machine = Arc::clone(&ctx.machine);
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            let _ = machine.hydrate().await;
            machine.snapshot()
        },
        move |snap| render(&ctx_render, &snap),
    );
}

/// Dispatch a fire-and-render action on the tokio runtime, then render the
/// resulting snapshot.
fn dispatch_action(ctx: &Rc<MailCtx>, action: MailPolicyAction) {
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

/// Render a `MailPolicySnapshot` into the page widgets (GTK main thread).
fn render(ctx: &Rc<MailCtx>, snap: &MailPolicySnapshot) {
    let w = &ctx.w;

    super::render_error_label(&w.error_label, snap.error.as_deref());

    // Mail-enable toggle — reflect persisted state without echoing back a dispatch.
    if w.enabled_toggle.is_active() != snap.mail_enabled {
        ctx.syncing.set(true);
        w.enabled_toggle.set_active(snap.mail_enabled);
        ctx.syncing.set(false);
    }

    // Auto-enable-for-new-users toggle — same echo-guarded persisted-state sync.
    if w.auto_enable_new_users.is_active() != snap.auto_enable_mail_for_new_users {
        ctx.syncing.set(true);
        w.auto_enable_new_users
            .set_active(snap.auto_enable_mail_for_new_users);
        ctx.syncing.set(false);
    }

    // Spam / inbound perimeter sub-struct. These controls have no per-change
    // dispatch (read on Save), so no echo guard is needed.
    let s = &snap.spam;
    set_u32(&w.threshold_junk, s.max_score_before_spam_folder);
    set_u32(&w.threshold_reject, s.max_score_before_reject);
    w.dnsbl_servers
        .buffer()
        .set_text(&s.dnsbl_servers.join("\n"));
    w.reject_no_rdns.set_active(s.reject_no_rdns);
    w.greylist_enabled.set_active(s.greylist_enabled);
    set_u32(&w.greylist_delay, s.greylist_delay_secs);
    set_u32(&w.max_conn_per_min, s.max_conn_per_min);
    w.fcrdns_mode
        .set_selected(FcrdnsMode::index_of_wire(&s.fcrdns_mode) as u32);
    w.helo_identity_required
        .set_active(s.helo_identity_required);
    w.reject_fcrdns_fail.set_active(s.reject_fcrdns_fail);
    set_u32(&w.max_message_bytes, s.max_message_bytes);
    set_u32(&w.bayesian_weight, s.bayesian_weight_milli);
    set_u32(&w.bayesian_min_samples, s.bayesian_min_samples);
    set_u32(
        &w.bayesian_full_confidence_samples,
        s.bayesian_full_confidence_samples,
    );
    set_u32(
        &w.training_history_retention,
        s.training_history_retention_days,
    );
    set_u32(&w.unlisted_recipient_penalty, s.unlisted_recipient_penalty);

    // Deployment-baseline publish outcome — hidden until the admin clicks
    // Publish. `published` comes straight from the shared BaselinePublishView,
    // so the client just picks the message + interpolates.
    match &snap.baseline_publish_result {
        Some(r) => {
            let mut msg = if r.published {
                S::spam_baseline_published(&r.contributors.to_string(), &r.sample_count.to_string())
            } else {
                S::spam_baseline_withheld(&r.contributors.to_string())
            };
            // The holder-side erosion count (silent-erosion fix, mail-spam.md
            // § Encrypted-mode interaction) — surfaced beside the published/
            // withheld message whenever the last run skipped anyone.
            if r.skipped_contributors > 0 {
                msg.push(' ');
                msg.push_str(&S::spam_baseline_skipped_contributors(
                    &r.skipped_contributors.to_string(),
                ));
            }
            // Text alone carries the state — never `set_visible`, which would take
            // the required element back out of the a11y tree (see its build site).
            w.publish_baseline_result.set_text(&msg);
        }
        None => {
            w.publish_baseline_result.set_text("");
        }
    }

    // Auth-enforcement sub-struct.
    let a = &snap.auth;
    w.enforce_dmarc.set_active(a.enforce_dmarc);
    w.enforce_dmarc_quarantine
        .set_active(a.enforce_dmarc_quarantine);
    w.enforce_spf_hardfail.set_active(a.enforce_spf_hardfail);
    w.enforce_dkim.set_active(a.enforce_dkim);
    w.log_only.set_active(a.log_only);
    set_u32(&w.max_failures, a.max_auth_failures_per_minute);
    set_u32(&w.max_conn_per_ip, a.max_conn_per_ip);

    // Submission-quota sub-struct.
    let sub = &snap.submission;
    set_u32(&w.submission_max_per_day, sub.max_per_day);
    set_u32(&w.submission_max_recipients, sub.max_recipients_per_message);

    // IMAP-server policy sub-struct.
    let i = &snap.imap;
    set_u32(&w.imap_idle_timeout, i.idle_timeout_secs);
    set_u32(&w.imap_tombstone_retention, i.tombstone_retention_days);
    w.imap_delete_nonempty
        .set_selected(ImapDeleteNonempty::index_of_wire(&i.delete_nonempty) as u32);
    set_u32(&w.imap_bodystructure_cache, i.bodystructure_cache_max);
    set_u64(&w.imap_storage_bytes, i.storage_bytes_default);
    set_u32(&w.imap_message_count, i.message_count_default);

    // Outbound-delivery sub-struct.
    let o = &snap.outbound;
    w.outbound_retry_schedule.buffer().set_text(
        &o.retry_schedule_seconds
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    );
    set_u32(
        &w.outbound_permfail_timeout,
        o.permanent_failure_timeout_hours,
    );
    set_u32(&w.outbound_delay_warning, o.delay_warning_at_hours);
    set_u32(&w.outbound_ndr_rate_limit, o.ndr_rate_limit_days);
    w.outbound_suppress_ndr_spf
        .set_active(o.suppress_ndr_spf_hardfail);
    w.outbound_suppress_ndr_dmarc
        .set_active(o.suppress_ndr_dmarc_reject);
    w.outbound_postmaster_cc.set_active(o.postmaster_cc_bounces);
    w.outbound_tlsrpt_send.set_active(o.tlsrpt_send_reports);
    w.outbound_ipv6.set_active(o.ipv6_enabled);
    w.outbound_treat_5xx_transient
        .buffer()
        .set_text(&o.treat_5xx_as_transient.join("\n"));

    // Alias-policy sub-struct (hydrated from the separate get_alias_policy twin,
    // not get_mail_config). Read on Save, so no echo guard needed.
    let al = &snap.alias;
    set_u32(&w.alias_exact_max, al.exact_aliases_max);
    w.alias_reserved_local_parts
        .buffer()
        .set_text(&al.reserved_local_parts.join("\n"));
    w.alias_subaddressing.set_active(al.subaddressing_enabled);
    w.alias_wildcard_prefix
        .set_active(al.wildcard_prefix_enabled);
}

/// Gather the whole Spam/inbound-perimeter sub-struct from the widgets, starting
/// from the persisted snapshot so unparsed/blank integer fields fall back to the
/// persisted value (full PUT — every field is sent).
fn gather_spam(ctx: &Rc<MailCtx>) -> SpamPolicyView {
    let w = &ctx.w;
    let mut s = ctx.machine.snapshot().spam;
    s.max_score_before_spam_folder = parse_u32(&w.threshold_junk, s.max_score_before_spam_folder);
    s.max_score_before_reject = parse_u32(&w.threshold_reject, s.max_score_before_reject);
    s.dnsbl_servers = read_lines(&w.dnsbl_servers);
    s.reject_no_rdns = w.reject_no_rdns.is_active();
    s.greylist_enabled = w.greylist_enabled.is_active();
    s.greylist_delay_secs = parse_u32(&w.greylist_delay, s.greylist_delay_secs);
    s.max_conn_per_min = parse_u32(&w.max_conn_per_min, s.max_conn_per_min);
    s.fcrdns_mode = FcrdnsMode::ORDER
        [(w.fcrdns_mode.selected() as usize).min(FcrdnsMode::ORDER.len() - 1)]
    .as_str()
    .to_string();
    s.helo_identity_required = w.helo_identity_required.is_active();
    s.reject_fcrdns_fail = w.reject_fcrdns_fail.is_active();
    s.max_message_bytes = parse_u32(&w.max_message_bytes, s.max_message_bytes);
    s.bayesian_weight_milli = parse_u32(&w.bayesian_weight, s.bayesian_weight_milli);
    s.bayesian_min_samples = parse_u32(&w.bayesian_min_samples, s.bayesian_min_samples);
    s.bayesian_full_confidence_samples = parse_u32(
        &w.bayesian_full_confidence_samples,
        s.bayesian_full_confidence_samples,
    );
    s.training_history_retention_days = parse_u32(
        &w.training_history_retention,
        s.training_history_retention_days,
    );
    s.unlisted_recipient_penalty =
        parse_u32(&w.unlisted_recipient_penalty, s.unlisted_recipient_penalty);
    s
}

/// Gather the whole Auth-enforcement sub-struct from the widgets (full PUT).
fn gather_auth(ctx: &Rc<MailCtx>) -> AuthPolicyView {
    let w = &ctx.w;
    let mut a = ctx.machine.snapshot().auth;
    a.enforce_dmarc = w.enforce_dmarc.is_active();
    a.enforce_dmarc_quarantine = w.enforce_dmarc_quarantine.is_active();
    a.enforce_spf_hardfail = w.enforce_spf_hardfail.is_active();
    a.enforce_dkim = w.enforce_dkim.is_active();
    a.log_only = w.log_only.is_active();
    a.max_auth_failures_per_minute = parse_u32(&w.max_failures, a.max_auth_failures_per_minute);
    a.max_conn_per_ip = parse_u32(&w.max_conn_per_ip, a.max_conn_per_ip);
    a
}

/// Gather the whole Submission-quota sub-struct from the widgets (full PUT).
fn gather_submission(ctx: &Rc<MailCtx>) -> SubmissionPolicyView {
    let w = &ctx.w;
    let mut s = ctx.machine.snapshot().submission;
    s.max_per_day = parse_u32(&w.submission_max_per_day, s.max_per_day);
    s.max_recipients_per_message =
        parse_u32(&w.submission_max_recipients, s.max_recipients_per_message);
    s
}

/// Gather the whole IMAP-server policy sub-struct from the widgets (full PUT).
fn gather_imap(ctx: &Rc<MailCtx>) -> ImapPolicyView {
    let w = &ctx.w;
    let mut i = ctx.machine.snapshot().imap;
    i.idle_timeout_secs = parse_u32(&w.imap_idle_timeout, i.idle_timeout_secs);
    i.tombstone_retention_days = parse_u32(&w.imap_tombstone_retention, i.tombstone_retention_days);
    i.delete_nonempty = ImapDeleteNonempty::ORDER
        [(w.imap_delete_nonempty.selected() as usize).min(ImapDeleteNonempty::ORDER.len() - 1)]
    .as_str()
    .to_string();
    i.bodystructure_cache_max = parse_u32(&w.imap_bodystructure_cache, i.bodystructure_cache_max);
    i.storage_bytes_default = parse_u64(&w.imap_storage_bytes, i.storage_bytes_default);
    i.message_count_default = parse_u32(&w.imap_message_count, i.message_count_default);
    i
}

/// Gather the whole Outbound-delivery sub-struct from the widgets (full PUT).
/// `postmaster_cc_bounces` is read-only (project policy never CC) — its switch is
/// insensitive, so `is_active()` returns the rendered persisted value unchanged.
fn gather_outbound(ctx: &Rc<MailCtx>) -> OutboundPolicyView {
    let w = &ctx.w;
    let mut o = ctx.machine.snapshot().outbound;
    o.retry_schedule_seconds =
        read_u64_lines(&w.outbound_retry_schedule, &o.retry_schedule_seconds);
    o.permanent_failure_timeout_hours = parse_u32(
        &w.outbound_permfail_timeout,
        o.permanent_failure_timeout_hours,
    );
    o.delay_warning_at_hours = parse_u32(&w.outbound_delay_warning, o.delay_warning_at_hours);
    o.ndr_rate_limit_days = parse_u32(&w.outbound_ndr_rate_limit, o.ndr_rate_limit_days);
    o.suppress_ndr_spf_hardfail = w.outbound_suppress_ndr_spf.is_active();
    o.suppress_ndr_dmarc_reject = w.outbound_suppress_ndr_dmarc.is_active();
    o.postmaster_cc_bounces = w.outbound_postmaster_cc.is_active();
    o.tlsrpt_send_reports = w.outbound_tlsrpt_send.is_active();
    o.ipv6_enabled = w.outbound_ipv6.is_active();
    o.treat_5xx_as_transient = read_lines(&w.outbound_treat_5xx_transient);
    o
}

/// Gather the whole alias-policy sub-struct from the widgets (full PUT). An
/// empty reserved-local-parts box clears the reservation (`Some(vec![])`),
/// matching the wire semantics — distinct from "keep the default".
fn gather_alias(ctx: &Rc<MailCtx>) -> AliasPolicyView {
    let w = &ctx.w;
    let mut a = ctx.machine.snapshot().alias;
    a.exact_aliases_max = parse_u32(&w.alias_exact_max, a.exact_aliases_max);
    a.reserved_local_parts = read_lines(&w.alias_reserved_local_parts);
    a.subaddressing_enabled = w.alias_subaddressing.is_active();
    a.wildcard_prefix_enabled = w.alias_wildcard_prefix.is_active();
    a
}

/// Set an entry's text to a `u32` value.
fn set_u32(entry: &gtk::Entry, value: u32) {
    entry.set_text(&value.to_string());
}

/// Set an entry's text to a `u64` value (IMAP storage-byte ceiling).
fn set_u64(entry: &gtk::Entry, value: u64) {
    entry.set_text(&value.to_string());
}

/// Parse an entry as `u32`, falling back to `prev` on an empty/unparseable value
/// (so a stray edit never silently zeroes a knob; mirrors `views/admin.rs::parse_cap`).
/// The trim/parse lives in the shared validator `fauna_core::format::parse_count`
/// (the one source of truth across all seven apps — `value-formatting.md`
/// § Mail-knob validation); this stays the thin `&gtk::Entry`→`&str` wrapper.
fn parse_u32(entry: &gtk::Entry, prev: u32) -> u32 {
    fauna_core::format::parse_count(&entry.text()).unwrap_or(prev)
}

/// Parse an entry as `u64`, falling back to `prev` (the IMAP storage ceiling).
/// Delegates to the shared `fauna_core::format::parse_count_u64` validator.
fn parse_u64(entry: &gtk::Entry, prev: u64) -> u64 {
    fauna_core::format::parse_count_u64(&entry.text()).unwrap_or(prev)
}

/// Read a multiline `TextView` as one trimmed non-blank `String` per line (the
/// dnsbl-servers + transient-5xx list knobs).
fn read_lines(view: &gtk::TextView) -> Vec<String> {
    let buffer = view.buffer();
    let text = buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), false)
        .to_string();
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// Read a multiline `TextView` as a `Vec<u64>` (one delay-seconds value per
/// non-blank line; the outbound retry schedule). Falls back to `prev` whole if
/// any line fails to parse (full-PUT replaces the list — never send a partially
/// parsed schedule).
fn read_u64_lines(view: &gtk::TextView, prev: &[u64]) -> Vec<u64> {
    let buffer = view.buffer();
    let text = buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), false)
        .to_string();
    let parsed: Option<Vec<u64>> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(fauna_core::format::parse_count_u64)
        .collect();
    parsed.unwrap_or_else(|| prev.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::testid::widget_names;

    /// The admin-mail page exposes every static ui.yaml ID for the page (the
    /// `admin-nav-back` button lives in the shared admin-shell header, not in this
    /// page's subtree, so it is not asserted here).
    #[test]
    fn admin_mail_page_exposes_static_ui_yaml_ids() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let page = build_admin_mail_page();
            let names = widget_names(&page);
            for id in [
                "page-heading",
                "error-message",
                "admin-mail-enabled-toggle",
                "admin-mail-auto-enable-new-users-toggle",
                "admin-mail-spam-threshold-junk",
                "admin-mail-spam-threshold-reject",
                "admin-mail-dnsbl-servers",
                "admin-mail-reject-no-rdns-toggle",
                "admin-mail-greylist-enabled-toggle",
                "admin-mail-greylist-delay-input",
                "admin-mail-max-conn-per-min-input",
                "admin-mail-fcrdns-mode-select",
                "admin-mail-helo-identity-required-toggle",
                "admin-mail-reject-fcrdns-fail-toggle",
                "admin-mail-max-message-bytes-input",
                "admin-mail-spam-bayesian-weight",
                "admin-mail-spam-bayesian-min-samples",
                "admin-mail-spam-bayesian-full-confidence-samples",
                "admin-mail-spam-training-history-retention",
                "admin-mail-unlisted-recipient-penalty",
                "admin-mail-spam-save-button",
                "admin-mail-publish-spam-baseline-button",
                "admin-mail-publish-spam-baseline-result",
                "admin-mail-auth-enforce-dmarc-toggle",
                "admin-mail-auth-enforce-dmarc-quarantine-toggle",
                "admin-mail-auth-enforce-spf-hardfail-toggle",
                "admin-mail-auth-enforce-dkim-toggle",
                "admin-mail-auth-log-only-toggle",
                "admin-mail-auth-max-failures-input",
                "admin-mail-auth-max-conn-per-ip-input",
                "admin-mail-auth-save-button",
                "admin-mail-submission-max-per-day-input",
                "admin-mail-submission-max-recipients-input",
                "admin-mail-submission-save-button",
                "admin-mail-imap-idle-timeout-input",
                "admin-mail-imap-tombstone-retention-input",
                "admin-mail-imap-delete-nonempty-select",
                "admin-mail-imap-bodystructure-cache-input",
                "admin-mail-imap-storage-bytes-input",
                "admin-mail-imap-message-count-input",
                "admin-mail-imap-save-button",
                "admin-mail-outbound-retry-schedule",
                "admin-mail-outbound-permfail-timeout-input",
                "admin-mail-outbound-delay-warning-input",
                "admin-mail-outbound-ndr-rate-limit-input",
                "admin-mail-outbound-suppress-ndr-spf-toggle",
                "admin-mail-outbound-suppress-ndr-dmarc-toggle",
                "admin-mail-outbound-postmaster-cc-toggle",
                "admin-mail-outbound-tlsrpt-send-toggle",
                "admin-mail-outbound-ipv6-toggle",
                "admin-mail-outbound-treat-5xx-transient",
                "admin-mail-outbound-save-button",
                "admin-mail-alias-exact-max-input",
                "admin-mail-alias-reserved-local-parts",
                "admin-mail-alias-subaddressing-toggle",
                "admin-mail-alias-wildcard-prefix-toggle",
                "admin-mail-alias-save-button",
            ] {
                assert!(
                    names.iter().any(|n| n == id),
                    "missing widget id {id:?}; have {names:?}",
                );
            }
        });
    }

    /// The DropDown's position↔wire-token mapping, exercised through the shared
    /// owner this file now calls. The *vocabulary* (order, tokens, unknown-token
    /// fallback) is pinned once in
    /// `fauna_client_mail_settings::admin_policy::picker_vocabulary_tests`; what
    /// stays worth asserting HERE is that this app's two position-addressed
    /// widgets are index-compatible with that order — a `set_selected` takes a
    /// raw position, so an off-by-one silently shows (and then saves) the wrong
    /// policy with no error anywhere.
    #[test]
    fn the_dropdowns_are_index_compatible_with_the_shared_order() {
        for (i, m) in FcrdnsMode::ORDER.iter().enumerate() {
            assert_eq!(FcrdnsMode::index_of_wire(m.as_str()), i);
        }
        assert!(
            FcrdnsMode::index_of_wire("bogus") < FcrdnsMode::ORDER.len(),
            "an unknown token must still land on a position this DropDown has"
        );
        for (i, m) in ImapDeleteNonempty::ORDER.iter().enumerate() {
            assert_eq!(ImapDeleteNonempty::index_of_wire(m.as_str()), i);
        }
        assert!(
            ImapDeleteNonempty::index_of_wire("bogus") < ImapDeleteNonempty::ORDER.len(),
            "an unknown token must still land on a position this DropDown has"
        );
    }
}
