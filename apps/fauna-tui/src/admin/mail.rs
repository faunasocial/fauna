//! The admin Mail sub-page (`admin-mail`) — the flat mail-*policy* form (`admin.md`
//! § 6 Mail; `mail-policy-config.md` § Policy catalog Tier 2).
//!
//! A dumb renderer of the shared `MailPolicyMachine`
//! (`fauna-client-mail-settings::admin_policy`): the two deployment-wide toggles
//! (`admin-mail-enabled-toggle` / `-auto-enable-new-users-toggle`) dispatch
//! immediately, and the six policy groups (spam / auth / submission / imap /
//! outbound / alias) each **full-PUT** their whole sub-struct on their own Save
//! button. The shell (`super`) owns the machine, ops and folds; this file is paint
//! only, plus the [`MailDrafts`] seed/gather impl (the six groups' edit drafts).
//!
//! Every editable control is a **draft** re-seeded from the persisted snapshot on
//! every fold (`super::apply_outcome`), so a group shows persisted state until the
//! human edits it and each Save gathers the group's drafts into the full `*View`
//! (unedited fields ride through unchanged — the no-clobber full-PUT; an unparsed
//! numeric falls back to the persisted value, mirroring linux's `gather_*`). A
//! machine dispatch/read error surfaces on the **global** `error-message` (`admin.md`
//! § 6 — unlike the page-scoped aliases error), bridged in the fold.

use fauna_client_mail_settings::admin_policy::{
    AliasPolicyView, AuthPolicyView, FcrdnsMode, ImapDeleteNonempty, ImapPolicyView,
    MailHealthView, MailPolicySnapshot, OutboundPolicyView, SpamPolicyView, SubmissionPolicyView,
};
use fauna_core::format::{parse_count, parse_count_u64};
use fauna_i18n::strings::admin as t;
use fauna_i18n::strings::common;
use fauna_ui_ids as ids;

use super::{
    Action, AdminField, AdminState, AliasDraft, AuthDraft, ImapDraft, MailDrafts, MailField,
    MailToggle, OutboundDraft, SpamDraft, SubmissionDraft,
};
use crate::element::{Element, Field, Gesture, SelectTarget};
use crate::pages::Page;

/// The wire values of the page's two raw-value pickers (the tokens round-trip
/// through `get_text`/`select`), read from their owner in shared Rust —
/// `fauna_client_mail_settings::admin_policy::{FcrdnsMode, ImapDeleteNonempty}`
/// — rather than restated here. linux held a byte-identical pair of arrays
/// until 2026-08-23, and Go a third copy of the FCrDNS one (priority #1/#2).
fn picker_values<const N: usize>(order: [&'static str; N]) -> Vec<String> {
    order.iter().map(|s| (*s).to_string()).collect()
}

pub(super) fn mail_elements(state: &AdminState) -> Vec<Element> {
    let d = &state.mail_drafts;
    let snap = state.mail_snapshot.as_ref();
    let mail_enabled = snap.map(|s| s.mail_enabled).unwrap_or(false);
    let auto_enable = snap
        .map(|s| s.auto_enable_mail_for_new_users)
        .unwrap_or(false);
    let postmaster_cc = snap
        .map(|s| s.outbound.postmaster_cc_bounces)
        .unwrap_or(false);

    let mut els = vec![
        Element::label(ids::PAGE_HEADING, t::mail_page::TITLE),
        Element::gesture_button(
            ids::ADMIN_NAV_BACK,
            t::EXIT,
            true,
            Gesture::Nav(Page::Conversations),
        )
        .nav_back(),
        // The two deployment-wide toggles dispatch immediately (non-optimistic:
        // `checked` reads the persisted snapshot, flipping only after the write +
        // re-read), NOT gathered into a group PUT.
        Element::checkbox_gesture(
            ids::ADMIN_MAIL_ENABLED_TOGGLE,
            t::mail_page::ENABLED_LABEL,
            mail_enabled,
            Gesture::Admin(Action::ToggleMailEnabled),
        ),
    ];
    // The mail health readout sits directly under the enable toggle — omitted
    // until the health read answers (a nest that refuses it gets no section,
    // never a guess).
    if let Some(health) = snap.and_then(|s| s.health.as_ref()) {
        els.extend(health_elements(
            health,
            state.mail_warmup_reset_armed,
            fauna_core::data::Timestamp::now_millis_or_zero() as i64,
        ));
    }
    els.push(Element::checkbox_gesture(
        ids::ADMIN_MAIL_AUTO_ENABLE_NEW_USERS_TOGGLE,
        t::mail_page::AUTO_ENABLE_NEW_USERS_LABEL,
        auto_enable,
        Gesture::Admin(Action::ToggleMailAutoEnable),
    ));

    // ── Spam / inbound perimeter (put_spam_policy) ──
    els.push(Element::chrome(t::mail_page::SPAM_GROUP_TITLE));
    els.push(text_input(
        state,
        "admin-mail-spam-threshold-junk",
        MailField::SpamJunk,
        t::mail_page::THRESHOLD_JUNK_LABEL,
    ));
    els.push(text_input(
        state,
        "admin-mail-spam-threshold-reject",
        MailField::SpamReject,
        t::mail_page::THRESHOLD_REJECT_LABEL,
    ));
    els.push(text_input(
        state,
        "admin-mail-dnsbl-servers",
        MailField::SpamDnsbl,
        t::mail_page::DNSBL_LABEL,
    ));
    els.push(toggle(
        "admin-mail-reject-no-rdns-toggle",
        t::mail_page::REJECT_NO_RDNS_LABEL,
        d.spam.reject_no_rdns,
        MailToggle::SpamRejectNoRdns,
    ));
    els.push(toggle(
        "admin-mail-greylist-enabled-toggle",
        t::mail_page::GREYLIST_ENABLED_LABEL,
        d.spam.greylist_enabled,
        MailToggle::SpamGreylistEnabled,
    ));
    els.push(text_input(
        state,
        "admin-mail-greylist-delay-input",
        MailField::SpamGreylistDelay,
        t::mail_page::GREYLIST_DELAY_LABEL,
    ));
    els.push(text_input(
        state,
        "admin-mail-max-conn-per-min-input",
        MailField::SpamMaxConnPerMin,
        t::mail_page::MAX_CONN_PER_MIN_LABEL,
    ));
    els.push(
        Element::select(
            ids::ADMIN_MAIL_FCRDNS_MODE_SELECT,
            d.spam.fcrdns_mode.clone(),
            SelectTarget::MailFcrdnsMode,
            picker_values(FcrdnsMode::ORDER.map(|m| m.as_str())),
        )
        .labelled(t::mail_page::FCRDNS_MODE_LABEL),
    );
    els.push(toggle(
        "admin-mail-helo-identity-required-toggle",
        t::mail_page::HELO_IDENTITY_LABEL,
        d.spam.helo_identity_required,
        MailToggle::SpamHeloIdentityRequired,
    ));
    els.push(toggle(
        "admin-mail-reject-fcrdns-fail-toggle",
        t::mail_page::REJECT_FCRDNS_FAIL_LABEL,
        d.spam.reject_fcrdns_fail,
        MailToggle::SpamRejectFcrdnsFail,
    ));
    els.push(text_input(
        state,
        "admin-mail-max-message-bytes-input",
        MailField::SpamMaxMessageBytes,
        t::mail_page::MAX_MESSAGE_BYTES_LABEL,
    ));
    els.push(text_input(
        state,
        "admin-mail-spam-bayesian-weight",
        MailField::SpamBayesianWeight,
        t::mail_page::BAYESIAN_WEIGHT_LABEL,
    ));
    els.push(text_input(
        state,
        "admin-mail-spam-bayesian-min-samples",
        MailField::SpamBayesianMinSamples,
        t::mail_page::BAYESIAN_MIN_SAMPLES_LABEL,
    ));
    els.push(text_input(
        state,
        "admin-mail-spam-bayesian-full-confidence-samples",
        MailField::SpamBayesianFullConfidence,
        t::mail_page::BAYESIAN_FULL_CONFIDENCE_SAMPLES_LABEL,
    ));
    els.push(text_input(
        state,
        "admin-mail-spam-training-history-retention",
        MailField::SpamTrainingRetention,
        t::mail_page::TRAINING_HISTORY_RETENTION_LABEL,
    ));
    els.push(text_input(
        state,
        "admin-mail-unlisted-recipient-penalty",
        MailField::SpamUnlistedPenalty,
        t::mail_page::UNLISTED_RECIPIENT_PENALTY_LABEL,
    ));
    els.push(save(
        "admin-mail-spam-save-button",
        t::mail_page::SPAM_SAVE,
        Action::SaveMailSpam,
    ));

    // ── Deployment baseline (admin opt-in aggregate) ──
    els.push(save(
        "admin-mail-publish-spam-baseline-button",
        t::mail_page::PUBLISH_SPAM_BASELINE_BUTTON,
        Action::PublishSpamBaseline,
    ));
    // The result is painted UNCONDITIONALLY (empty text until Publish is clicked —
    // it registers on its non-empty id, `ui::register_frame`), so
    // `count("admin-mail-publish-spam-baseline-result")` is true on a fresh page.
    els.push(Element::label(
        ids::ADMIN_MAIL_PUBLISH_SPAM_BASELINE_RESULT,
        baseline_result_text(snap),
    ));
    // Standing publish is its own gesture (dispatched immediately, `checked`
    // off the persisted snapshot like the two deployment-wide toggles), not a
    // Spam-group draft — flipping it off withdraws the served baseline.
    els.push(Element::checkbox_gesture(
        ids::ADMIN_MAIL_SPAM_BASELINE_STANDING_TOGGLE,
        t::mail_page::SPAM_BASELINE_STANDING_LABEL,
        snap.map(|s| s.spam.baseline_standing_publish)
            .unwrap_or(false),
        Gesture::Admin(Action::ToggleMailBaselineStanding),
    ));
    // Painted unconditionally, like the result above (blank until the state
    // read answers).
    els.push(Element::label(
        ids::ADMIN_MAIL_SPAM_BASELINE_STATE,
        baseline_state_text(snap),
    ));

    // ── Inbound authentication enforcement (put_auth_policy) ──
    els.push(Element::chrome(t::mail_page::AUTH_GROUP_TITLE));
    els.push(toggle(
        "admin-mail-auth-enforce-dmarc-toggle",
        t::mail_page::ENFORCE_DMARC_LABEL,
        d.auth.enforce_dmarc,
        MailToggle::AuthEnforceDmarc,
    ));
    els.push(toggle(
        "admin-mail-auth-enforce-dmarc-quarantine-toggle",
        t::mail_page::ENFORCE_DMARC_QUARANTINE_LABEL,
        d.auth.enforce_dmarc_quarantine,
        MailToggle::AuthEnforceDmarcQuarantine,
    ));
    els.push(toggle(
        "admin-mail-auth-enforce-spf-hardfail-toggle",
        t::mail_page::ENFORCE_SPF_HARDFAIL_LABEL,
        d.auth.enforce_spf_hardfail,
        MailToggle::AuthEnforceSpfHardfail,
    ));
    els.push(toggle(
        "admin-mail-auth-enforce-dkim-toggle",
        t::mail_page::ENFORCE_DKIM_LABEL,
        d.auth.enforce_dkim,
        MailToggle::AuthEnforceDkim,
    ));
    els.push(toggle(
        "admin-mail-auth-log-only-toggle",
        t::mail_page::LOG_ONLY_LABEL,
        d.auth.log_only,
        MailToggle::AuthLogOnly,
    ));
    els.push(text_input(
        state,
        "admin-mail-auth-max-failures-input",
        MailField::AuthMaxFailures,
        t::mail_page::MAX_FAILURES_LABEL,
    ));
    els.push(text_input(
        state,
        "admin-mail-auth-max-conn-per-ip-input",
        MailField::AuthMaxConnPerIp,
        t::mail_page::MAX_CONN_PER_IP_LABEL,
    ));
    els.push(save(
        "admin-mail-auth-save-button",
        t::mail_page::AUTH_SAVE,
        Action::SaveMailAuth,
    ));

    // ── Submission quotas (put_submission_policy) ──
    els.push(Element::chrome(t::mail_page::SUBMISSION_GROUP_TITLE));
    els.push(text_input(
        state,
        "admin-mail-submission-max-per-day-input",
        MailField::SubmissionMaxPerDay,
        t::mail_page::SUBMISSION_MAX_PER_DAY_LABEL,
    ));
    els.push(text_input(
        state,
        "admin-mail-submission-max-recipients-input",
        MailField::SubmissionMaxRecipients,
        t::mail_page::SUBMISSION_MAX_RECIPIENTS_LABEL,
    ));
    els.push(save(
        "admin-mail-submission-save-button",
        t::mail_page::SUBMISSION_SAVE,
        Action::SaveMailSubmission,
    ));

    // ── IMAP server policy (put_imap_policy) ──
    els.push(Element::chrome(t::mail_page::IMAP_GROUP_TITLE));
    els.push(text_input(
        state,
        "admin-mail-imap-idle-timeout-input",
        MailField::ImapIdleTimeout,
        t::mail_page::IMAP_IDLE_TIMEOUT_LABEL,
    ));
    els.push(text_input(
        state,
        "admin-mail-imap-tombstone-retention-input",
        MailField::ImapTombstoneRetention,
        t::mail_page::IMAP_TOMBSTONE_RETENTION_LABEL,
    ));
    els.push(
        Element::select(
            ids::ADMIN_MAIL_IMAP_DELETE_NONEMPTY_SELECT,
            d.imap.delete_nonempty.clone(),
            SelectTarget::MailImapDelete,
            picker_values(ImapDeleteNonempty::ORDER.map(|m| m.as_str())),
        )
        .labelled(t::mail_page::IMAP_DELETE_NONEMPTY_LABEL),
    );
    els.push(text_input(
        state,
        "admin-mail-imap-bodystructure-cache-input",
        MailField::ImapBodystructureCache,
        t::mail_page::IMAP_BODYSTRUCTURE_CACHE_LABEL,
    ));
    els.push(text_input(
        state,
        "admin-mail-imap-storage-bytes-input",
        MailField::ImapStorageBytes,
        t::mail_page::IMAP_STORAGE_BYTES_LABEL,
    ));
    els.push(text_input(
        state,
        "admin-mail-imap-message-count-input",
        MailField::ImapMessageCount,
        t::mail_page::IMAP_MESSAGE_COUNT_LABEL,
    ));
    els.push(save(
        "admin-mail-imap-save-button",
        t::mail_page::IMAP_SAVE,
        Action::SaveMailImap,
    ));

    // ── Outbound delivery (put_outbound_policy) ──
    els.push(Element::chrome(t::mail_page::OUTBOUND_GROUP_TITLE));
    els.push(text_input(
        state,
        "admin-mail-outbound-retry-schedule",
        MailField::OutboundRetrySchedule,
        t::mail_page::OUTBOUND_RETRY_SCHEDULE_LABEL,
    ));
    els.push(text_input(
        state,
        "admin-mail-outbound-permfail-timeout-input",
        MailField::OutboundPermfailTimeout,
        t::mail_page::OUTBOUND_PERMFAIL_TIMEOUT_LABEL,
    ));
    els.push(text_input(
        state,
        "admin-mail-outbound-delay-warning-input",
        MailField::OutboundDelayWarning,
        t::mail_page::OUTBOUND_DELAY_WARNING_LABEL,
    ));
    els.push(text_input(
        state,
        "admin-mail-outbound-ndr-rate-limit-input",
        MailField::OutboundNdrRateLimit,
        t::mail_page::OUTBOUND_NDR_RATE_LIMIT_LABEL,
    ));
    els.push(toggle(
        "admin-mail-outbound-suppress-ndr-spf-toggle",
        t::mail_page::OUTBOUND_SUPPRESS_NDR_SPF_LABEL,
        d.outbound.suppress_ndr_spf,
        MailToggle::OutboundSuppressNdrSpf,
    ));
    els.push(toggle(
        "admin-mail-outbound-suppress-ndr-dmarc-toggle",
        t::mail_page::OUTBOUND_SUPPRESS_NDR_DMARC_LABEL,
        d.outbound.suppress_ndr_dmarc,
        MailToggle::OutboundSuppressNdrDmarc,
    ));
    // Read-only — project policy never CC (`admin.md` § 6). A terminal has no
    // greyed-out-switch chrome, and a fake-interactive checkbox would be the
    // dropped-command anti-pattern (`testing.md` point 11), so this is a value
    // display: the persisted state as a labelled Enabled/Disabled line.
    els.push(
        Element::label(
            ids::ADMIN_MAIL_OUTBOUND_POSTMASTER_CC_TOGGLE,
            if postmaster_cc {
                common::ENABLED
            } else {
                common::DISABLED
            },
        )
        .labelled(t::mail_page::OUTBOUND_POSTMASTER_CC_LABEL),
    );
    els.push(toggle(
        "admin-mail-outbound-tlsrpt-send-toggle",
        t::mail_page::OUTBOUND_TLSRPT_SEND_LABEL,
        d.outbound.tlsrpt_send,
        MailToggle::OutboundTlsrptSend,
    ));
    els.push(toggle(
        "admin-mail-outbound-ipv6-toggle",
        t::mail_page::OUTBOUND_IPV6_LABEL,
        d.outbound.ipv6,
        MailToggle::OutboundIpv6,
    ));
    els.push(text_input(
        state,
        "admin-mail-outbound-treat-5xx-transient",
        MailField::OutboundTreat5xx,
        t::mail_page::OUTBOUND_TREAT_5XX_LABEL,
    ));
    els.push(save(
        "admin-mail-outbound-save-button",
        t::mail_page::OUTBOUND_SAVE,
        Action::SaveMailOutbound,
    ));

    // ── Aliases (put_alias_policy; nest-side, dual-read via get_alias_policy) ──
    els.push(Element::chrome(t::mail_page::ALIAS_GROUP_TITLE));
    els.push(text_input(
        state,
        "admin-mail-alias-exact-max-input",
        MailField::AliasExactMax,
        t::mail_page::ALIAS_EXACT_MAX_LABEL,
    ));
    els.push(text_input(
        state,
        "admin-mail-alias-reserved-local-parts",
        MailField::AliasReservedLocalParts,
        t::mail_page::ALIAS_RESERVED_LABEL,
    ));
    els.push(toggle(
        "admin-mail-alias-subaddressing-toggle",
        t::mail_page::ALIAS_SUBADDRESSING_LABEL,
        d.alias.subaddressing,
        MailToggle::AliasSubaddressing,
    ));
    els.push(toggle(
        "admin-mail-alias-wildcard-prefix-toggle",
        t::mail_page::ALIAS_WILDCARD_PREFIX_LABEL,
        d.alias.wildcard_prefix,
        MailToggle::AliasWildcardPrefix,
    ));
    els.push(save(
        "admin-mail-alias-save-button",
        t::mail_page::ALIAS_SAVE,
        Action::SaveMailAlias,
    ));

    els
}

/// The mail health readout (`mail-deliverability.md` § The mail health readout)
/// — a dumb render of the shared fold: every sentence is shared Rust
/// (`fauna_core::format::mail_health_status_text` / `_check_state_label` /
/// `_heartbeat_text`) or the nest's own row detail; tui decides nothing.
///
/// Flat-indexed rows, the `admin-stat-card` idiom: a bare container marker
/// (empty text) before each row's three leaves, so `-label`/`-state`/`-detail`
/// at index *i* are row *i*'s. The two heartbeat rows (5 and 6) carry an empty
/// `detail` on purpose — the stamp is painted there. The warm-up reset relabels
/// to its confirm sentence while armed (the two-click confirm's whole visible
/// affordance); the delist link shows its URL, since a terminal copies rather
/// than opens.
fn health_elements(health: &MailHealthView, warmup_armed: bool, now_ms: i64) -> Vec<Element> {
    let lookup = fauna_i18n::strings::lookup;
    let mut els = vec![
        Element::label(ids::ADMIN_MAIL_HEALTH_SECTION, t::mail_page::HEALTH_TITLE),
        Element::label(
            ids::ADMIN_MAIL_HEALTH_STATUS,
            fauna_core::format::mail_health_status_text(
                &health.state,
                health.last_outbound_delivered_at,
                health.last_inbound_accepted_at,
                now_ms,
                lookup,
            ),
        ),
    ];
    let heartbeats = [
        health.last_outbound_delivered_at,
        health.last_inbound_accepted_at,
    ];
    for (i, check) in health.checks.iter().enumerate() {
        let detail = match i.checked_sub(5).and_then(|h| heartbeats.get(h)) {
            Some(stamp) => fauna_core::format::mail_health_heartbeat_text(*stamp, now_ms, lookup),
            None => check.detail.clone(),
        };
        els.push(Element::label(ids::ADMIN_MAIL_HEALTH_CHECK, String::new()));
        els.push(Element::label(
            ids::ADMIN_MAIL_HEALTH_CHECK_LABEL,
            lookup(&check.label_key).unwrap_or(check.label_key.as_str()),
        ));
        els.push(Element::label(
            ids::ADMIN_MAIL_HEALTH_CHECK_STATE,
            fauna_core::format::mail_health_check_state_label(&check.state).resolve(lookup),
        ));
        els.push(Element::label(ids::ADMIN_MAIL_HEALTH_CHECK_DETAIL, detail));
    }
    if let Some(url) = &health.delist_url {
        els.push(Element::gesture_button(
            ids::ADMIN_MAIL_HEALTH_DELIST_LINK,
            format!("{}: {url}", t::mail_page::HEALTH_DELIST),
            true,
            Gesture::Admin(Action::CopyMailDelistUrl { url: url.clone() }),
        ));
    }
    els.push(Element::gesture_button(
        ids::ADMIN_MAIL_HEALTH_RECHECK_BUTTON,
        t::mail_page::HEALTH_RECHECK,
        true,
        Gesture::Admin(Action::RecheckMailHealth),
    ));
    els.push(Element::gesture_button(
        ids::ADMIN_MAIL_HEALTH_WARMUP_RESET_BUTTON,
        if warmup_armed {
            t::mail_page::HEALTH_WARMUP_RESET_CONFIRM
        } else {
            t::mail_page::HEALTH_WARMUP_RESET
        },
        true,
        Gesture::Admin(Action::ResetMailWarmup),
    ));
    els
}

/// One numeric/list text input, showing the current group draft; a keystroke
/// writes it via `AdminField::Mail`, gathered into the group's PUT on Save.
fn text_input(state: &AdminState, id: &str, field: MailField, label: &str) -> Element {
    Element::input(
        id,
        state.mail_drafts.text(field),
        Field::Admin(AdminField::Mail(field)),
    )
    .labelled(label)
}

/// One in-group toggle — its `checked` reads the group draft (not the persisted
/// snapshot), flipping locally on click and gathered into the group's PUT on Save.
fn toggle(id: &str, label: &str, checked: bool, which: MailToggle) -> Element {
    Element::checkbox_gesture(
        id,
        label,
        checked,
        Gesture::Admin(Action::ToggleMail(which)),
    )
}

/// One group's Save button (always enabled — a save gathers whatever the drafts hold).
fn save(id: &str, label: &str, action: Action) -> Element {
    Element::gesture_button(id, label, true, Gesture::Admin(action))
}

/// The `admin-mail-publish-spam-baseline-result` text — empty until Publish is
/// clicked (`baseline_publish_result` is stashed on the snapshot with no re-read),
/// then the published / k-anonymity-withheld message + an optional skipped-merge note.
fn baseline_result_text(snap: Option<&MailPolicySnapshot>) -> String {
    let Some(result) = snap.and_then(|s| s.baseline_publish_result.as_ref()) else {
        return String::new();
    };
    // `deferred` first: a delta-floor deferral also carries `published = false`,
    // and must never read as "too few contributors".
    let mut line = if result.deferred {
        t::mail_page::SPAM_BASELINE_WAITING.to_string()
    } else if result.published {
        t::mail_page::spam_baseline_published(
            &result.contributors.to_string(),
            &result.sample_count.to_string(),
        )
    } else {
        t::mail_page::spam_baseline_withheld(&result.contributors.to_string())
    };
    if result.skipped_contributors > 0 {
        line.push(' ');
        line.push_str(&t::mail_page::spam_baseline_skipped_contributors(
            &result.skipped_contributors.to_string(),
        ));
    }
    line
}

/// The `admin-mail-spam-baseline-state` text — the served baseline's current
/// state (`mail-spam.md` § Cold start Path 2 → *Standing publish*): "Published
/// over N contributors on <date>." or "No baseline published.", followed by
/// "Waiting for more contributor activity." when the last run was deferred.
/// Blank until the state read answers (and on a nest that refuses it).
fn baseline_state_text(snap: Option<&MailPolicySnapshot>) -> String {
    let Some(state) = snap.and_then(|s| s.baseline_state.as_ref()) else {
        return String::new();
    };
    let mut line = match (state.published, state.published_at_ms) {
        (true, Some(at_ms)) => t::mail_page::spam_baseline_state_published(
            &state.contributors.to_string(),
            &fauna_core::format::format_unix_local_date_ms(at_ms),
        ),
        _ => t::mail_page::SPAM_BASELINE_STATE_NONE.to_string(),
    };
    if state.deferred {
        line.push(' ');
        line.push_str(t::mail_page::SPAM_BASELINE_WAITING);
    }
    line
}

impl MailDrafts {
    /// Re-seed every group's drafts from the persisted snapshot — numeric/list
    /// fields to their string form, toggles/selects to their value. Called on every
    /// fold so a group always shows persisted state until the human edits again.
    pub(super) fn seed(&mut self, snap: &MailPolicySnapshot) {
        let s = &snap.spam;
        self.spam = SpamDraft {
            junk: s.max_score_before_spam_folder.to_string(),
            reject: s.max_score_before_reject.to_string(),
            dnsbl: s.dnsbl_servers.join("\n"),
            reject_no_rdns: s.reject_no_rdns,
            greylist_enabled: s.greylist_enabled,
            greylist_delay: s.greylist_delay_secs.to_string(),
            max_conn_per_min: s.max_conn_per_min.to_string(),
            fcrdns_mode: s.fcrdns_mode.clone(),
            helo_identity_required: s.helo_identity_required,
            reject_fcrdns_fail: s.reject_fcrdns_fail,
            max_message_bytes: s.max_message_bytes.to_string(),
            bayesian_weight: s.bayesian_weight_milli.to_string(),
            bayesian_min_samples: s.bayesian_min_samples.to_string(),
            bayesian_full_confidence: s.bayesian_full_confidence_samples.to_string(),
            training_retention: s.training_history_retention_days.to_string(),
            unlisted_penalty: s.unlisted_recipient_penalty.to_string(),
        };
        let a = &snap.auth;
        self.auth = AuthDraft {
            enforce_dmarc: a.enforce_dmarc,
            enforce_dmarc_quarantine: a.enforce_dmarc_quarantine,
            enforce_spf_hardfail: a.enforce_spf_hardfail,
            enforce_dkim: a.enforce_dkim,
            log_only: a.log_only,
            max_failures: a.max_auth_failures_per_minute.to_string(),
            max_conn_per_ip: a.max_conn_per_ip.to_string(),
        };
        let sub = &snap.submission;
        self.submission = SubmissionDraft {
            max_per_day: sub.max_per_day.to_string(),
            max_recipients: sub.max_recipients_per_message.to_string(),
        };
        let i = &snap.imap;
        self.imap = ImapDraft {
            idle_timeout: i.idle_timeout_secs.to_string(),
            tombstone_retention: i.tombstone_retention_days.to_string(),
            delete_nonempty: i.delete_nonempty.clone(),
            bodystructure_cache: i.bodystructure_cache_max.to_string(),
            storage_bytes: i.storage_bytes_default.to_string(),
            message_count: i.message_count_default.to_string(),
        };
        let o = &snap.outbound;
        self.outbound = OutboundDraft {
            retry_schedule: join_u64(&o.retry_schedule_seconds),
            permfail_timeout: o.permanent_failure_timeout_hours.to_string(),
            delay_warning: o.delay_warning_at_hours.to_string(),
            ndr_rate_limit: o.ndr_rate_limit_days.to_string(),
            suppress_ndr_spf: o.suppress_ndr_spf_hardfail,
            suppress_ndr_dmarc: o.suppress_ndr_dmarc_reject,
            tlsrpt_send: o.tlsrpt_send_reports,
            ipv6: o.ipv6_enabled,
            treat_5xx: o.treat_5xx_as_transient.join("\n"),
        };
        let al = &snap.alias;
        self.alias = AliasDraft {
            exact_max: al.exact_aliases_max.to_string(),
            reserved_local_parts: al.reserved_local_parts.join("\n"),
            subaddressing: al.subaddressing_enabled,
            wildcard_prefix: al.wildcard_prefix_enabled,
        };
    }

    /// Read a text-input draft (the automation agent's `get`, the keyboard's read).
    pub(super) fn text(&self, field: MailField) -> String {
        use MailField::*;
        match field {
            SpamJunk => self.spam.junk.clone(),
            SpamReject => self.spam.reject.clone(),
            SpamDnsbl => self.spam.dnsbl.clone(),
            SpamGreylistDelay => self.spam.greylist_delay.clone(),
            SpamMaxConnPerMin => self.spam.max_conn_per_min.clone(),
            SpamMaxMessageBytes => self.spam.max_message_bytes.clone(),
            SpamBayesianWeight => self.spam.bayesian_weight.clone(),
            SpamBayesianMinSamples => self.spam.bayesian_min_samples.clone(),
            SpamBayesianFullConfidence => self.spam.bayesian_full_confidence.clone(),
            SpamTrainingRetention => self.spam.training_retention.clone(),
            SpamUnlistedPenalty => self.spam.unlisted_penalty.clone(),
            AuthMaxFailures => self.auth.max_failures.clone(),
            AuthMaxConnPerIp => self.auth.max_conn_per_ip.clone(),
            SubmissionMaxPerDay => self.submission.max_per_day.clone(),
            SubmissionMaxRecipients => self.submission.max_recipients.clone(),
            ImapIdleTimeout => self.imap.idle_timeout.clone(),
            ImapTombstoneRetention => self.imap.tombstone_retention.clone(),
            ImapBodystructureCache => self.imap.bodystructure_cache.clone(),
            ImapStorageBytes => self.imap.storage_bytes.clone(),
            ImapMessageCount => self.imap.message_count.clone(),
            OutboundRetrySchedule => self.outbound.retry_schedule.clone(),
            OutboundPermfailTimeout => self.outbound.permfail_timeout.clone(),
            OutboundDelayWarning => self.outbound.delay_warning.clone(),
            OutboundNdrRateLimit => self.outbound.ndr_rate_limit.clone(),
            OutboundTreat5xx => self.outbound.treat_5xx.clone(),
            AliasExactMax => self.alias.exact_max.clone(),
            AliasReservedLocalParts => self.alias.reserved_local_parts.clone(),
        }
    }

    /// Write a text-input draft — a keystroke or the agent's `/element/type`.
    pub(super) fn set_text(&mut self, field: MailField, value: String) {
        use MailField::*;
        match field {
            SpamJunk => self.spam.junk = value,
            SpamReject => self.spam.reject = value,
            SpamDnsbl => self.spam.dnsbl = value,
            SpamGreylistDelay => self.spam.greylist_delay = value,
            SpamMaxConnPerMin => self.spam.max_conn_per_min = value,
            SpamMaxMessageBytes => self.spam.max_message_bytes = value,
            SpamBayesianWeight => self.spam.bayesian_weight = value,
            SpamBayesianMinSamples => self.spam.bayesian_min_samples = value,
            SpamBayesianFullConfidence => self.spam.bayesian_full_confidence = value,
            SpamTrainingRetention => self.spam.training_retention = value,
            SpamUnlistedPenalty => self.spam.unlisted_penalty = value,
            AuthMaxFailures => self.auth.max_failures = value,
            AuthMaxConnPerIp => self.auth.max_conn_per_ip = value,
            SubmissionMaxPerDay => self.submission.max_per_day = value,
            SubmissionMaxRecipients => self.submission.max_recipients = value,
            ImapIdleTimeout => self.imap.idle_timeout = value,
            ImapTombstoneRetention => self.imap.tombstone_retention = value,
            ImapBodystructureCache => self.imap.bodystructure_cache = value,
            ImapStorageBytes => self.imap.storage_bytes = value,
            ImapMessageCount => self.imap.message_count = value,
            OutboundRetrySchedule => self.outbound.retry_schedule = value,
            OutboundPermfailTimeout => self.outbound.permfail_timeout = value,
            OutboundDelayWarning => self.outbound.delay_warning = value,
            OutboundNdrRateLimit => self.outbound.ndr_rate_limit = value,
            OutboundTreat5xx => self.outbound.treat_5xx = value,
            AliasExactMax => self.alias.exact_max = value,
            AliasReservedLocalParts => self.alias.reserved_local_parts = value,
        }
    }

    /// Flip one in-group toggle draft.
    pub(super) fn toggle(&mut self, which: MailToggle) {
        use MailToggle::*;
        match which {
            SpamRejectNoRdns => self.spam.reject_no_rdns = !self.spam.reject_no_rdns,
            SpamGreylistEnabled => self.spam.greylist_enabled = !self.spam.greylist_enabled,
            SpamHeloIdentityRequired => {
                self.spam.helo_identity_required = !self.spam.helo_identity_required
            }
            SpamRejectFcrdnsFail => self.spam.reject_fcrdns_fail = !self.spam.reject_fcrdns_fail,
            AuthEnforceDmarc => self.auth.enforce_dmarc = !self.auth.enforce_dmarc,
            AuthEnforceDmarcQuarantine => {
                self.auth.enforce_dmarc_quarantine = !self.auth.enforce_dmarc_quarantine
            }
            AuthEnforceSpfHardfail => {
                self.auth.enforce_spf_hardfail = !self.auth.enforce_spf_hardfail
            }
            AuthEnforceDkim => self.auth.enforce_dkim = !self.auth.enforce_dkim,
            AuthLogOnly => self.auth.log_only = !self.auth.log_only,
            OutboundSuppressNdrSpf => {
                self.outbound.suppress_ndr_spf = !self.outbound.suppress_ndr_spf
            }
            OutboundSuppressNdrDmarc => {
                self.outbound.suppress_ndr_dmarc = !self.outbound.suppress_ndr_dmarc
            }
            OutboundTlsrptSend => self.outbound.tlsrpt_send = !self.outbound.tlsrpt_send,
            OutboundIpv6 => self.outbound.ipv6 = !self.outbound.ipv6,
            AliasSubaddressing => self.alias.subaddressing = !self.alias.subaddressing,
            AliasWildcardPrefix => self.alias.wildcard_prefix = !self.alias.wildcard_prefix,
        }
    }

    /// Gather the Spam group's drafts into a full `SpamPolicyView`. Numeric fields
    /// parse over the persisted `base` value (an unparsed/blank field rides through
    /// unchanged — the no-clobber full-PUT, linux's `gather_spam`).
    pub(super) fn gather_spam(&self, base: &SpamPolicyView) -> SpamPolicyView {
        let d = &self.spam;
        SpamPolicyView {
            max_score_before_spam_folder: pu32(&d.junk, base.max_score_before_spam_folder),
            max_score_before_reject: pu32(&d.reject, base.max_score_before_reject),
            dnsbl_servers: split_lines(&d.dnsbl),
            reject_no_rdns: d.reject_no_rdns,
            greylist_enabled: d.greylist_enabled,
            greylist_delay_secs: pu32(&d.greylist_delay, base.greylist_delay_secs),
            max_conn_per_min: pu32(&d.max_conn_per_min, base.max_conn_per_min),
            fcrdns_mode: d.fcrdns_mode.clone(),
            helo_identity_required: d.helo_identity_required,
            reject_fcrdns_fail: d.reject_fcrdns_fail,
            max_message_bytes: pu32(&d.max_message_bytes, base.max_message_bytes),
            bayesian_weight_milli: pu32(&d.bayesian_weight, base.bayesian_weight_milli),
            bayesian_min_samples: pu32(&d.bayesian_min_samples, base.bayesian_min_samples),
            bayesian_full_confidence_samples: pu32(
                &d.bayesian_full_confidence,
                base.bayesian_full_confidence_samples,
            ),
            training_history_retention_days: pu32(
                &d.training_retention,
                base.training_history_retention_days,
            ),
            unlisted_recipient_penalty: pu32(&d.unlisted_penalty, base.unlisted_recipient_penalty),
            // Not a Spam-group draft (the standing toggle is its own gesture) —
            // the persisted value rides through, so a group save never turns
            // the deployment's standing baseline publish off (which withdraws).
            baseline_standing_publish: base.baseline_standing_publish,
        }
    }

    /// Gather the Auth group's drafts into a full `AuthPolicyView`.
    pub(super) fn gather_auth(&self, base: &AuthPolicyView) -> AuthPolicyView {
        let d = &self.auth;
        AuthPolicyView {
            enforce_dmarc: d.enforce_dmarc,
            enforce_dmarc_quarantine: d.enforce_dmarc_quarantine,
            enforce_spf_hardfail: d.enforce_spf_hardfail,
            enforce_dkim: d.enforce_dkim,
            log_only: d.log_only,
            max_auth_failures_per_minute: pu32(&d.max_failures, base.max_auth_failures_per_minute),
            max_conn_per_ip: pu32(&d.max_conn_per_ip, base.max_conn_per_ip),
        }
    }

    /// Gather the Submission group's drafts into a full `SubmissionPolicyView`.
    pub(super) fn gather_submission(&self, base: &SubmissionPolicyView) -> SubmissionPolicyView {
        let d = &self.submission;
        SubmissionPolicyView {
            max_per_day: pu32(&d.max_per_day, base.max_per_day),
            max_recipients_per_message: pu32(&d.max_recipients, base.max_recipients_per_message),
        }
    }

    /// Gather the IMAP group's drafts into a full `ImapPolicyView`.
    pub(super) fn gather_imap(&self, base: &ImapPolicyView) -> ImapPolicyView {
        let d = &self.imap;
        ImapPolicyView {
            idle_timeout_secs: pu32(&d.idle_timeout, base.idle_timeout_secs),
            tombstone_retention_days: pu32(&d.tombstone_retention, base.tombstone_retention_days),
            delete_nonempty: d.delete_nonempty.clone(),
            bodystructure_cache_max: pu32(&d.bodystructure_cache, base.bodystructure_cache_max),
            storage_bytes_default: pu64(&d.storage_bytes, base.storage_bytes_default),
            message_count_default: pu32(&d.message_count, base.message_count_default),
        }
    }

    /// Gather the Outbound group's drafts into a full `OutboundPolicyView`.
    /// `postmaster_cc_bounces` is carried through from the base unchanged (read-only).
    pub(super) fn gather_outbound(&self, base: &OutboundPolicyView) -> OutboundPolicyView {
        let d = &self.outbound;
        OutboundPolicyView {
            retry_schedule_seconds: split_u64_lines(
                &d.retry_schedule,
                &base.retry_schedule_seconds,
            ),
            permanent_failure_timeout_hours: pu32(
                &d.permfail_timeout,
                base.permanent_failure_timeout_hours,
            ),
            delay_warning_at_hours: pu32(&d.delay_warning, base.delay_warning_at_hours),
            ndr_rate_limit_days: pu32(&d.ndr_rate_limit, base.ndr_rate_limit_days),
            suppress_ndr_spf_hardfail: d.suppress_ndr_spf,
            suppress_ndr_dmarc_reject: d.suppress_ndr_dmarc,
            postmaster_cc_bounces: base.postmaster_cc_bounces,
            tlsrpt_send_reports: d.tlsrpt_send,
            ipv6_enabled: d.ipv6,
            treat_5xx_as_transient: split_lines(&d.treat_5xx),
        }
    }

    /// Gather the Alias group's drafts into a full `AliasPolicyView`.
    pub(super) fn gather_alias(&self, base: &AliasPolicyView) -> AliasPolicyView {
        let d = &self.alias;
        AliasPolicyView {
            exact_aliases_max: pu32(&d.exact_max, base.exact_aliases_max),
            reserved_local_parts: split_lines(&d.reserved_local_parts),
            subaddressing_enabled: d.subaddressing,
            wildcard_prefix_enabled: d.wildcard_prefix,
        }
    }
}

/// Parse a numeric draft over the persisted value (unparsed/blank → keep persisted;
/// the shared `parse_count`, the linux `parse_u32` fallback).
fn pu32(draft: &str, prev: u32) -> u32 {
    parse_count(draft.trim()).unwrap_or(prev)
}

/// Parse a u64 numeric draft over the persisted value (IMAP storage bytes).
fn pu64(draft: &str, prev: u64) -> u64 {
    parse_count_u64(draft.trim()).unwrap_or(prev)
}

/// Split a one-value-per-line list draft (trim, drop blanks) → full-replace `Vec`.
/// An empty box → `vec![]`, a meaningful override that clears the list.
fn split_lines(draft: &str) -> Vec<String> {
    draft
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

/// Split a one-value-per-line u64 list draft (retry schedule). If ANY line fails to
/// parse, keep the whole persisted list (never send a partially-parsed schedule —
/// linux's `read_u64_lines`).
fn split_u64_lines(draft: &str, prev: &[u64]) -> Vec<u64> {
    let mut out = Vec::new();
    for line in draft.lines().map(str::trim).filter(|l| !l.is_empty()) {
        match parse_count_u64(line) {
            Some(v) => out.push(v),
            None => return prev.to_vec(),
        }
    }
    out
}

/// Join a u64 list for display (one per line).
fn join_u64(values: &[u64]) -> String {
    values
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use fauna_client_mail_settings::admin_policy::{
        BaselinePublishView, BaselineStateView, MailHealthCheckView, MailPolicyStatus,
    };

    use super::*;
    use crate::admin::AdminPage;

    fn sample_snapshot() -> MailPolicySnapshot {
        MailPolicySnapshot {
            mail_enabled: true,
            auto_enable_mail_for_new_users: true,
            spam: SpamPolicyView {
                max_score_before_spam_folder: 5,
                max_score_before_reject: 0,
                dnsbl_servers: vec!["zen.example".into()],
                reject_no_rdns: false,
                greylist_enabled: true,
                greylist_delay_secs: 60,
                max_conn_per_min: 30,
                fcrdns_mode: "score_signal".into(),
                helo_identity_required: false,
                reject_fcrdns_fail: false,
                max_message_bytes: 26_214_400,
                bayesian_weight_milli: 700,
                bayesian_min_samples: 50,
                bayesian_full_confidence_samples: 200,
                training_history_retention_days: 30,
                unlisted_recipient_penalty: 0,
                baseline_standing_publish: false,
            },
            auth: AuthPolicyView {
                enforce_dmarc: true,
                enforce_dmarc_quarantine: false,
                enforce_spf_hardfail: false,
                enforce_dkim: false,
                log_only: false,
                max_auth_failures_per_minute: 10,
                max_conn_per_ip: 256,
            },
            submission: SubmissionPolicyView {
                max_per_day: 500,
                max_recipients_per_message: 100,
            },
            imap: ImapPolicyView {
                idle_timeout_secs: 1740,
                tombstone_retention_days: 30,
                delete_nonempty: "forbidden".into(),
                bodystructure_cache_max: 128,
                storage_bytes_default: 1_073_741_824,
                message_count_default: 100_000,
            },
            outbound: OutboundPolicyView {
                retry_schedule_seconds: vec![60, 300, 900],
                permanent_failure_timeout_hours: 120,
                delay_warning_at_hours: 4,
                ndr_rate_limit_days: 1,
                suppress_ndr_spf_hardfail: false,
                suppress_ndr_dmarc_reject: false,
                postmaster_cc_bounces: false,
                tlsrpt_send_reports: false,
                ipv6_enabled: false,
                treat_5xx_as_transient: vec![],
            },
            alias: AliasPolicyView {
                exact_aliases_max: 20,
                reserved_local_parts: vec!["postmaster".into()],
                subaddressing_enabled: true,
                wildcard_prefix_enabled: false,
            },
            status: MailPolicyStatus::Idle,
            baseline_publish_result: None,
            baseline_state: None,
            health: Some(sample_health()),
            error: None,
        }
    }

    /// A readout as the nest's fold returns it: seven rows in the fixed order,
    /// the two heartbeat rows with an empty `detail`, nothing listed.
    fn sample_health() -> MailHealthView {
        let row = |label_key: &str, state: &str, detail: &str| MailHealthCheckView {
            label_key: label_key.into(),
            state: state.into(),
            detail: detail.into(),
        };
        MailHealthView {
            state: "warming_up".into(),
            checks: vec![
                row(
                    "admin.mail_page.health_check_bridge",
                    "pass",
                    "1 of 1 connected",
                ),
                row(
                    "admin.mail_page.health_check_blocklist",
                    "pass",
                    "not listed",
                ),
                row(
                    "admin.mail_page.health_check_queue",
                    "pass",
                    "no delayed messages",
                ),
                row(
                    "admin.mail_page.health_check_records",
                    "warn",
                    "some checks could not complete",
                ),
                row("admin.mail_page.health_check_warmup", "info", "day 3 of 30"),
                row("admin.mail_page.health_check_last_delivered", "info", ""),
                row("admin.mail_page.health_check_last_received", "info", ""),
            ],
            last_outbound_delivered_at: None,
            last_inbound_accepted_at: None,
            delist_url: None,
        }
    }

    fn tagged_ids(els: &[Element]) -> Vec<&str> {
        els.iter()
            .map(|e| e.id.as_str())
            .filter(|id| !id.is_empty())
            .collect()
    }

    /// The page paints its ui.yaml `elements` in order — the two deployment toggles,
    /// then the six policy groups (each its inputs/toggles/select then its Save),
    /// with the publish button + result and the read-only postmaster-cc value.
    #[test]
    fn mail_page_paints_ids_in_ui_yaml_order() {
        let mut app = crate::app::tests::test_app();
        app.admin.sub = AdminPage::Mail;
        super::super::apply_outcome(
            &mut app,
            super::super::Outcome::MailSnapshot(Box::new(sample_snapshot())),
        );

        assert_eq!(
            tagged_ids(&mail_elements(&app.admin)),
            vec![
                "page-heading",
                "admin-nav-back",
                "admin-mail-enabled-toggle",
                // The health readout, directly under the enable toggle.
                "admin-mail-health-section",
                "admin-mail-health-status",
                "admin-mail-health-check",
                "admin-mail-health-check-label",
                "admin-mail-health-check-state",
                "admin-mail-health-check-detail",
                "admin-mail-health-check",
                "admin-mail-health-check-label",
                "admin-mail-health-check-state",
                "admin-mail-health-check-detail",
                "admin-mail-health-check",
                "admin-mail-health-check-label",
                "admin-mail-health-check-state",
                "admin-mail-health-check-detail",
                "admin-mail-health-check",
                "admin-mail-health-check-label",
                "admin-mail-health-check-state",
                "admin-mail-health-check-detail",
                "admin-mail-health-check",
                "admin-mail-health-check-label",
                "admin-mail-health-check-state",
                "admin-mail-health-check-detail",
                "admin-mail-health-check",
                "admin-mail-health-check-label",
                "admin-mail-health-check-state",
                "admin-mail-health-check-detail",
                "admin-mail-health-check",
                "admin-mail-health-check-label",
                "admin-mail-health-check-state",
                "admin-mail-health-check-detail",
                "admin-mail-health-recheck-button",
                "admin-mail-health-warmup-reset-button",
                "admin-mail-auto-enable-new-users-toggle",
                // Spam
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
                "admin-mail-spam-baseline-standing-toggle",
                "admin-mail-spam-baseline-state",
                // Auth
                "admin-mail-auth-enforce-dmarc-toggle",
                "admin-mail-auth-enforce-dmarc-quarantine-toggle",
                "admin-mail-auth-enforce-spf-hardfail-toggle",
                "admin-mail-auth-enforce-dkim-toggle",
                "admin-mail-auth-log-only-toggle",
                "admin-mail-auth-max-failures-input",
                "admin-mail-auth-max-conn-per-ip-input",
                "admin-mail-auth-save-button",
                // Submission
                "admin-mail-submission-max-per-day-input",
                "admin-mail-submission-max-recipients-input",
                "admin-mail-submission-save-button",
                // IMAP
                "admin-mail-imap-idle-timeout-input",
                "admin-mail-imap-tombstone-retention-input",
                "admin-mail-imap-delete-nonempty-select",
                "admin-mail-imap-bodystructure-cache-input",
                "admin-mail-imap-storage-bytes-input",
                "admin-mail-imap-message-count-input",
                "admin-mail-imap-save-button",
                // Outbound
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
                // Alias
                "admin-mail-alias-exact-max-input",
                "admin-mail-alias-reserved-local-parts",
                "admin-mail-alias-subaddressing-toggle",
                "admin-mail-alias-wildcard-prefix-toggle",
                "admin-mail-alias-save-button",
            ]
        );

        // A field hydrates from the persisted snapshot (junk → its catalog default 5).
        let junk = mail_elements(&app.admin)
            .into_iter()
            .find(|e| e.id == "admin-mail-spam-threshold-junk")
            .expect("junk painted");
        assert_eq!(junk.text, "5");
    }

    fn texts<'a>(els: &'a [Element], id: &str) -> Vec<&'a str> {
        els.iter()
            .filter(|e| e.id == id)
            .map(|e| e.text.as_str())
            .collect()
    }

    /// The readout paints the shared sentences: the status line opens with the
    /// state's label and carries both heartbeats, each row resolves its label
    /// key and verdict, and the heartbeat rows show "Never" where the fold left
    /// the detail empty. No health read yet → no section at all.
    #[test]
    fn health_section_paints_the_shared_fold() {
        let els = health_elements(&sample_health(), false, 1_700_000_000_000);
        let status = texts(&els, "admin-mail-health-status");
        assert_eq!(
            status,
            vec!["Mail: warming up — last delivered: Never · last received: Never"]
        );
        assert_eq!(
            texts(&els, "admin-mail-health-check-label"),
            vec![
                "Mail service connection",
                "Blocklist check",
                "Outgoing queue",
                "DNS and authentication records",
                "Sending warm-up",
                "Last delivered",
                "Last received",
            ]
        );
        assert_eq!(
            texts(&els, "admin-mail-health-check-state"),
            vec!["OK", "OK", "OK", "Warning", "Info", "Info", "Info"]
        );
        let details = texts(&els, "admin-mail-health-check-detail");
        assert_eq!(details[4], "day 3 of 30", "a nest detail renders verbatim");
        assert_eq!(
            &details[5..],
            &["Never", "Never"],
            "heartbeats fill the empty detail"
        );
        assert!(texts(&els, "admin-mail-health-delist-link").is_empty());

        let mut app = crate::app::tests::test_app();
        let mut bare = sample_snapshot();
        bare.health = None;
        super::super::apply_outcome(
            &mut app,
            super::super::Outcome::MailSnapshot(Box::new(bare)),
        );
        assert!(
            !tagged_ids(&mail_elements(&app.admin)).contains(&"admin-mail-health-section"),
            "no health read → no section"
        );
    }

    /// A listing shows the delist link with its URL; the warm-up reset relabels
    /// to its confirm sentence while armed.
    #[test]
    fn health_section_delist_link_and_armed_reset() {
        let mut health = sample_health();
        health.delist_url = Some("https://check.spamhaus.org/".into());
        let els = health_elements(&health, true, 0);
        let link = texts(&els, "admin-mail-health-delist-link");
        assert_eq!(link.len(), 1);
        assert!(link[0].ends_with("https://check.spamhaus.org/"), "{link:?}");
        assert_eq!(
            texts(&els, "admin-mail-health-warmup-reset-button"),
            vec![t::mail_page::HEALTH_WARMUP_RESET_CONFIRM]
        );
    }

    /// The warm-up reset is a two-click confirm: the first press only arms (no
    /// op), the second disarms and dispatches, and a mail fold never leaves it
    /// armed.
    #[test]
    fn warmup_reset_arms_then_dispatches_and_a_fold_disarms() {
        let mut app = crate::app::tests::test_app();
        app.admin.sub = AdminPage::Mail;
        super::super::apply_outcome(
            &mut app,
            super::super::Outcome::MailSnapshot(Box::new(sample_snapshot())),
        );
        assert!(!app.admin.mail_warmup_reset_armed);

        assert!(super::super::apply_local(&mut app, Action::ResetMailWarmup).is_none());
        assert!(app.admin.mail_warmup_reset_armed, "first press arms only");

        let op = super::super::apply_local(&mut app, Action::ResetMailWarmup);
        assert!(!app.admin.mail_warmup_reset_armed, "the confirm disarms");
        // The test app builds no machine, so there is no op to hand out — what
        // matters is that the confirm reached the dispatch arm (disarmed) rather
        // than arming again.
        drop(op);

        app.admin.mail_warmup_reset_armed = true;
        super::super::apply_outcome(
            &mut app,
            super::super::Outcome::MailSnapshot(Box::new(sample_snapshot())),
        );
        assert!(!app.admin.mail_warmup_reset_armed, "a re-read disarms");
    }

    /// A mail-snapshot fold re-seeds every group's drafts from persisted state and
    /// bridges the snapshot error onto the global `error-message`; a clean snapshot
    /// clears it.
    #[test]
    fn mail_snapshot_fold_seeds_drafts_and_bridges_error() {
        let mut app = crate::app::tests::test_app();
        let mut failing = sample_snapshot();
        failing.error = Some("bad threshold order".into());
        super::super::apply_outcome(
            &mut app,
            super::super::Outcome::MailSnapshot(Box::new(failing)),
        );
        assert_eq!(
            app.errors.get(&Page::Admin).map(String::as_str),
            Some("bad threshold order"),
        );
        // Drafts seeded across groups.
        assert_eq!(app.admin.mail_drafts.spam.junk, "5");
        assert_eq!(app.admin.mail_drafts.auth.max_conn_per_ip, "256");
        assert_eq!(app.admin.mail_drafts.imap.storage_bytes, "1073741824");
        assert_eq!(
            app.admin.mail_drafts.outbound.retry_schedule,
            "60\n300\n900"
        );
        assert_eq!(app.admin.mail_drafts.alias.exact_max, "20");
        assert!(app.admin.mail_drafts.spam.greylist_enabled);

        // A clean snapshot clears the error.
        super::super::apply_outcome(
            &mut app,
            super::super::Outcome::MailSnapshot(Box::new(sample_snapshot())),
        );
        assert!(!app.errors.contains_key(&Page::Admin));
    }

    /// A group Save gathers the whole `*View` from the drafts over the persisted
    /// base: an edited numeric is carried, an unparseable one falls back to the
    /// persisted value, and the sibling (unedited) fields ride through unchanged.
    #[test]
    fn gather_spam_carries_edits_with_fallback_and_no_clobber() {
        let mut drafts = MailDrafts::default();
        let base = sample_snapshot().spam;
        drafts.seed(&sample_snapshot());

        // Edit the junk threshold; the rest keep their seeded (persisted) values.
        drafts.spam.junk = "7".into();
        let gathered = drafts.gather_spam(&base);
        assert_eq!(
            gathered.max_score_before_spam_folder, 7,
            "edited field carried"
        );
        assert_eq!(
            gathered.bayesian_weight_milli, 700,
            "sibling field rides through unchanged (no clobber)"
        );

        // An unparseable numeric falls back to the persisted base value.
        drafts.spam.junk = "not-a-number".into();
        assert_eq!(
            drafts.gather_spam(&base).max_score_before_spam_folder,
            base.max_score_before_spam_folder,
        );
    }

    /// The publish-baseline result renders empty until a result is stashed, then the
    /// k-anonymity-withheld message (the e2e's "too few contributors" assertion).
    #[test]
    fn baseline_result_renders_withheld_message() {
        let mut snap = sample_snapshot();
        assert_eq!(baseline_result_text(Some(&snap)), "");
        snap.baseline_publish_result = Some(BaselinePublishView {
            published: false,
            contributors: 0,
            sample_count: 0,
            skipped_contributors: 0,
            deferred: false,
        });
        assert!(
            baseline_result_text(Some(&snap))
                .to_lowercase()
                .contains("too few contributors")
        );
    }

    /// A delta-floor deferral also carries `published = false`; it must render
    /// the waiting line, never the withheld one.
    #[test]
    fn baseline_result_renders_deferred_as_waiting() {
        let mut snap = sample_snapshot();
        snap.baseline_publish_result = Some(BaselinePublishView {
            published: false,
            contributors: 3,
            sample_count: 0,
            skipped_contributors: 0,
            deferred: true,
        });
        assert_eq!(
            baseline_result_text(Some(&snap)),
            "Waiting for more contributor activity."
        );
    }

    /// The state text: blank before the read answers, the two sentences, and
    /// the waiting suffix on either.
    #[test]
    fn baseline_state_renders_published_none_and_waiting() {
        let mut snap = sample_snapshot();
        assert_eq!(baseline_state_text(Some(&snap)), "");
        let published_at = 1_758_000_000_000;
        let mut state = BaselineStateView {
            published: true,
            contributors: 4,
            sample_count: 90,
            published_at_ms: Some(published_at),
            skipped_contributors: 0,
            deferred: false,
            standing: true,
        };
        snap.baseline_state = Some(state.clone());
        assert_eq!(
            baseline_state_text(Some(&snap)),
            format!(
                "Published over 4 contributors on {}.",
                fauna_core::format::format_unix_local_date_ms(published_at)
            )
        );
        state.deferred = true;
        snap.baseline_state = Some(state.clone());
        assert!(
            baseline_state_text(Some(&snap)).ends_with(". Waiting for more contributor activity.")
        );
        snap.baseline_state = Some(BaselineStateView {
            published: false,
            contributors: 0,
            sample_count: 0,
            published_at_ms: None,
            ..state
        });
        assert_eq!(
            baseline_state_text(Some(&snap)),
            "No baseline published. Waiting for more contributor activity."
        );
    }

    /// A list draft splits on newlines dropping blanks (full-replace); the u64
    /// retry-schedule keeps the persisted list if any line fails to parse.
    #[test]
    fn list_gathers_split_and_fall_back() {
        assert_eq!(
            split_lines("a.test\n\n  b.test  \n"),
            vec!["a.test".to_string(), "b.test".to_string()]
        );
        assert!(split_lines("   ").is_empty(), "empty box clears the list");
        assert_eq!(split_u64_lines("30\n90", &[1, 2]), vec![30, 90]);
        assert_eq!(
            split_u64_lines("30\nnope", &[1, 2]),
            vec![1, 2],
            "a bad line keeps the persisted schedule"
        );
    }
}
