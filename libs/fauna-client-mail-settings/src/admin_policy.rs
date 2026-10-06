//! Shared orchestration for the flat **`admin-mail`** page — the admin-tier
//! mail-*policy* form (Tier 2 of `docs/goal/behavior/mail-policy-config.md`).
//!
//! Authority for behavior + the policy catalog: `mail-policy-config.md`
//! § Policy catalog (Tier 2) + § Implementation status today (which knobs have a
//! live write-path). Authority for UX/IDs: `docs/goal/behavior/admin.md`
//! § Mail (`admin-mail`) + `tests/e2e-unified/ui.yaml` `admin-mail`.
//!
//! Unlike the user-tier `mail-settings` machines (which use the User-class
//! `MailAccountClient`), this is **Admin-class** — it wraps `MailAdminClient`
//! (`libs/fauna-client-bridges`), mirroring `local_domains.rs` / `forwarders.rs`.
//! The UI renders [`MailPolicySnapshot`] and dispatches [`MailPolicyAction`]; the
//! per-app glue implements one WS-RPC seam ([`MailPolicyNest`]).
//!
//! **Read + write are both live.** The form *hydrates* via the admin read twin
//! `fauna.bridges.get_mail_config` (the overlaid effective config; `Refresh`),
//! and *saves* via the Admin-class write kinds — `set_mail_enabled` and the
//! per-sub-struct `put_<substruct>_policy` (full PUT, not a merge: the form
//! submits the whole sub-struct so each field is sent `Some(_)`). nest rejects an
//! out-of-order spam-threshold write (`fauna.protocol.malformed`); the machine
//! surfaces it via [`MailPolicySnapshot::error`], never faked green.
//!
//! Scope today: `mail.enabled` + all five projected write-path sub-structs —
//! **Spam/Inbound** (`put_spam_policy`), **Auth** (`put_auth_policy`),
//! **Submission** (`put_submission_policy`), **IMAP** (`put_imap_policy`), and
//! **Outbound** (`put_outbound_policy`) — covering every widget archetype the page
//! needs (bool toggle, integer field, string dropdown, string-list, integer-list).
//! The nest-side **alias** policy (`put_alias_policy`) needs its own read twin
//! (`get_alias_policy`, owed) before it folds in.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fauna_protocol::MaybeSendSync;
use fauna_protocol::bridge_routing::{
    AliasPolicy, AuthPolicy, FetchConfigReply, GetSpamBaselineStateReply, ImapPolicy,
    MailHealthCheck, MailHealthReply, OutboundPolicy, PublishSpamBaselineReply,
    PutAliasPolicyRequest, PutAuthPolicyRequest, PutImapPolicyRequest, PutOutboundPolicyRequest,
    PutSpamPolicyRequest, PutSubmissionPolicyRequest, SpamPolicyThresholds,
    SubmissionPolicyThresholds,
};
use serde::{Deserialize, Serialize};

use fauna_core::format::ReachPolicyOption;
use fauna_core::localized::LocalizedText;

use crate::error::{DispatchError, NestError};

/// The Spam/Inbound-perimeter policy sub-struct as the `admin-mail` form renders
/// and edits it. Field-identical to the wire [`SpamPolicyThresholds`] (a *local*
/// view so the UI/FFI surface is decoupled from the wire type and uniffi-local,
/// mirroring `ForwarderView`). `mail-policy-config.md` § Inbound perimeter.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct SpamPolicyView {
    /// Combined-score threshold to deliver to Junk (`0` = disabled).
    pub max_score_before_spam_folder: u32,
    /// Combined-score threshold to 554-reject (`0` = disabled, default).
    pub max_score_before_reject: u32,
    /// DNS blocklists queried during RCPT/DATA (full-replace on save).
    pub dnsbl_servers: Vec<String>,
    /// Reject senders with no rDNS entry.
    pub reject_no_rdns: bool,
    /// Greylist first-seen senders.
    pub greylist_enabled: bool,
    /// Hold-down before a re-attempt is accepted, seconds.
    pub greylist_delay_secs: u32,
    /// Per-peer-IP connection ceiling per 60 s window.
    pub max_conn_per_min: u32,
    /// FCrDNS mode: `"off"` / `"score_signal"` / `"enforce"`.
    pub fcrdns_mode: String,
    /// Require the HELO argument to A-resolve to the peer IP.
    pub helo_identity_required: bool,
    /// Reject outright on FCrDNS failure (only meaningful in `enforce`).
    pub reject_fcrdns_fail: bool,
    /// Pre-parser inbound size cap, bytes (bodies above → `552 5.3.4`).
    pub max_message_bytes: u32,
    /// Per-user Bayesian weight in the combined-score formula, carried as milli
    /// to avoid a float wire field (`700` = 0.7). Rendered as a 0–1000 integer.
    /// `mail-spam.md` § Combined-score formula.
    pub bayesian_weight_milli: u32,
    /// Cold-start sample floor below which the per-user term is 0 (default 50).
    pub bayesian_min_samples: u32,
    /// Sample count at which the per-user confidence ramp reaches 1.0 and the
    /// cold-start baseline fade reaches 0 (default 200). nest rejects an effective
    /// value `<= bayesian_min_samples` (a collapsed ramp) with `fauna.protocol.
    /// malformed`, surfaced via [`MailPolicySnapshot::error`].
    pub bayesian_full_confidence_samples: u32,
    /// Per-message training-audit retention in days (default 30); nest GC's older
    /// `spam_training_history` rows daily. `mail-spam.md` § Training-sample retention.
    pub training_history_retention_days: u32,
    /// Recipient-whitelist unlisted-recipient penalty in **points**, added to a
    /// catch-all recipient's combined score at the Go MTA loop. **`0` = off
    /// (default)**. `mail-spam.md` § Unlisted-recipient penalty.
    pub unlisted_recipient_penalty: u32,
    /// Standing publish of the deployment spam baseline (**default off**): on,
    /// the nest republishes every 24 hours under both floors; off, it stops and
    /// withdraws the baseline. `mail-spam.md` § Cold start Path 2 → *Standing
    /// publish*. A form that does not render it yet must carry the hydrated
    /// value through unchanged — a full-PUT of `false` over `true` withdraws.
    /// `#[serde(default)]` for a JSON-bridged app built before the field.
    #[serde(default)]
    pub baseline_standing_publish: bool,
}

impl From<SpamPolicyThresholds> for SpamPolicyView {
    fn from(p: SpamPolicyThresholds) -> Self {
        Self {
            max_score_before_spam_folder: p.max_score_before_spam_folder,
            max_score_before_reject: p.max_score_before_reject,
            dnsbl_servers: p.dnsbl_servers,
            reject_no_rdns: p.reject_no_rdns,
            greylist_enabled: p.greylist_enabled,
            greylist_delay_secs: p.greylist_delay_secs,
            max_conn_per_min: p.max_conn_per_min,
            fcrdns_mode: p.fcrdns_mode,
            helo_identity_required: p.helo_identity_required,
            reject_fcrdns_fail: p.reject_fcrdns_fail,
            max_message_bytes: p.max_message_bytes,
            bayesian_weight_milli: p.bayesian_weight_milli,
            bayesian_min_samples: p.bayesian_min_samples,
            bayesian_full_confidence_samples: p.bayesian_full_confidence_samples,
            training_history_retention_days: p.training_history_retention_days,
            unlisted_recipient_penalty: p.unlisted_recipient_penalty,
            baseline_standing_publish: p.baseline_standing_publish,
        }
    }
}

impl SpamPolicyView {
    /// Full-PUT request: the form edits the whole sub-struct, so every field is
    /// sent `Some(_)` (a replace, not a merge — `PutSpamPolicyRequest` doc).
    fn into_put_request(self) -> PutSpamPolicyRequest {
        PutSpamPolicyRequest {
            max_score_before_spam_folder: Some(self.max_score_before_spam_folder),
            max_score_before_reject: Some(self.max_score_before_reject),
            dnsbl_servers: Some(self.dnsbl_servers),
            reject_no_rdns: Some(self.reject_no_rdns),
            greylist_enabled: Some(self.greylist_enabled),
            greylist_delay_secs: Some(self.greylist_delay_secs),
            max_conn_per_min: Some(self.max_conn_per_min),
            fcrdns_mode: Some(self.fcrdns_mode),
            helo_identity_required: Some(self.helo_identity_required),
            reject_fcrdns_fail: Some(self.reject_fcrdns_fail),
            max_message_bytes: Some(self.max_message_bytes),
            // The `admin-mail` Spam group now renders these four Tier-2 per-user
            // knobs (`mail-policy-config.md` § Spam), so the whole sub-struct
            // full-PUTs together — every field `Some(_)`. `gather_spam` seeds the
            // form from the hydrated snapshot, so an unedited perimeter save
            // re-sends the persisted bayesian values (no clobber).
            bayesian_weight_milli: Some(self.bayesian_weight_milli),
            bayesian_min_samples: Some(self.bayesian_min_samples),
            bayesian_full_confidence_samples: Some(self.bayesian_full_confidence_samples),
            training_history_retention_days: Some(self.training_history_retention_days),
            unlisted_recipient_penalty: Some(self.unlisted_recipient_penalty),
            baseline_standing_publish: self.baseline_standing_publish,
            // Forward-compat catch-all (transport.md rule 4); nothing to
            // carry forward on a locally-built request.
            extra: Default::default(),
        }
    }
}

/// The Auth/enforcement policy sub-struct as the `admin-mail` form renders + edits
/// it. Field-identical to the wire [`AuthPolicy`]. `mail-policy-config.md`
/// § Submission policy (DMARC/SPF/DKIM enforcement gates + AUTH-failure lockout).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AuthPolicyView {
    /// Enforce a DMARC `p=reject` verdict at DATA.
    pub enforce_dmarc: bool,
    /// Enforce a DMARC `p=quarantine` verdict (deliver to Junk).
    pub enforce_dmarc_quarantine: bool,
    /// Reject on an SPF hardfail (`-all`).
    pub enforce_spf_hardfail: bool,
    /// Reject DKIM-fail when no DMARC `p=` decision applies (off by default —
    /// many legitimate senders ship unsigned mail).
    pub enforce_dkim: bool,
    /// Compute verdicts but never reject (onboarding aid).
    pub log_only: bool,
    /// Per-(credential, source-IP) submission AUTH-failure ceiling per minute.
    pub max_auth_failures_per_minute: u32,
    /// Per-source-IP concurrent-connection cap on the authenticated
    /// submission/IMAP/CalDAV listeners (`0` = disabled). Bridge-enforced via
    /// `internal/connlimit`; complements the AUTH-failure lockout above.
    pub max_conn_per_ip: u32,
}

impl From<AuthPolicy> for AuthPolicyView {
    fn from(p: AuthPolicy) -> Self {
        Self {
            enforce_dmarc: p.enforce_dmarc,
            enforce_dmarc_quarantine: p.enforce_dmarc_quarantine,
            enforce_spf_hardfail: p.enforce_spf_hardfail,
            enforce_dkim: p.enforce_dkim,
            log_only: p.log_only,
            max_auth_failures_per_minute: p.max_auth_failures_per_minute,
            max_conn_per_ip: p.max_conn_per_ip,
        }
    }
}

impl AuthPolicyView {
    fn into_put_request(self) -> PutAuthPolicyRequest {
        PutAuthPolicyRequest {
            enforce_dmarc: Some(self.enforce_dmarc),
            enforce_dmarc_quarantine: Some(self.enforce_dmarc_quarantine),
            enforce_spf_hardfail: Some(self.enforce_spf_hardfail),
            enforce_dkim: Some(self.enforce_dkim),
            log_only: Some(self.log_only),
            max_auth_failures_per_minute: Some(self.max_auth_failures_per_minute),
            max_conn_per_ip: Some(self.max_conn_per_ip),
            // Forward-compat catch-all (transport.md rule 4); nothing to
            // carry forward on a locally-built request.
            extra: Default::default(),
        }
    }
}

/// The Submission-quota policy sub-struct as the `admin-mail` form renders + edits
/// it. Field-identical to the wire [`SubmissionPolicyThresholds`] (the two
/// projected + admin-writable submission thresholds — distinct from the
/// catalog's still-aspirational per-actor-rate rows). `mail-policy-config.md`
/// § Submission policy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct SubmissionPolicyView {
    /// Per-actor outbound-submission ceiling per day (sealed into the submission
    /// token as `MaxMessagesPerDay`).
    pub max_per_day: u32,
    /// Per-message recipient ceiling (sealed as `MaxRecipients`).
    pub max_recipients_per_message: u32,
}

impl From<SubmissionPolicyThresholds> for SubmissionPolicyView {
    fn from(p: SubmissionPolicyThresholds) -> Self {
        Self {
            max_per_day: p.max_per_day,
            max_recipients_per_message: p.max_recipients_per_message,
        }
    }
}

impl SubmissionPolicyView {
    fn into_put_request(self) -> PutSubmissionPolicyRequest {
        PutSubmissionPolicyRequest {
            max_per_day: Some(self.max_per_day),
            max_recipients_per_message: Some(self.max_recipients_per_message),
            // Forward-compat catch-all (transport.md rule 4); nothing to
            // carry forward on a locally-built request.
            extra: Default::default(),
        }
    }
}

/// The IMAP-server policy sub-struct as the `admin-mail` form renders + edits it.
/// Field-identical to the wire [`ImapPolicy`]. `mail-policy-config.md`
/// § IMAP server policy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ImapPolicyView {
    /// IDLE inactivity timeout the MDA enforces against MUA sessions (seconds).
    pub idle_timeout_secs: u32,
    /// QRESYNC tombstone retention window (days; nest floors at 7).
    pub tombstone_retention_days: u32,
    /// DELETE policy for non-empty mailboxes: `"forbidden"` / `"allowed"`.
    pub delete_nonempty: String,
    /// MDA BodyStructure/Envelope derivation-cache size (entries).
    pub bodystructure_cache_max: u32,
    /// Per-actor STORAGE quota ceiling (bytes).
    pub storage_bytes_default: u64,
    /// Per-actor MESSAGE-count quota ceiling.
    pub message_count_default: u32,
}

impl From<ImapPolicy> for ImapPolicyView {
    fn from(p: ImapPolicy) -> Self {
        Self {
            idle_timeout_secs: p.idle_timeout_secs,
            tombstone_retention_days: p.tombstone_retention_days,
            delete_nonempty: p.delete_nonempty,
            bodystructure_cache_max: p.bodystructure_cache_max,
            storage_bytes_default: p.storage_bytes_default,
            message_count_default: p.message_count_default,
        }
    }
}

impl ImapPolicyView {
    fn into_put_request(self) -> PutImapPolicyRequest {
        PutImapPolicyRequest {
            idle_timeout_secs: Some(self.idle_timeout_secs),
            tombstone_retention_days: Some(self.tombstone_retention_days),
            delete_nonempty: Some(self.delete_nonempty),
            bodystructure_cache_max: Some(self.bodystructure_cache_max),
            storage_bytes_default: Some(self.storage_bytes_default),
            message_count_default: Some(self.message_count_default),
            // Forward-compat catch-all (transport.md rule 4); nothing to
            // carry forward on a locally-built request.
            extra: Default::default(),
        }
    }
}

/// The Outbound-delivery policy sub-struct as the `admin-mail` form renders +
/// edits it. Field-identical to the wire [`OutboundPolicy`]; the list fields
/// full-replace on save. `mail-policy-config.md` § Outbound delivery.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct OutboundPolicyView {
    /// Delays before each successive retry attempt (seconds; one per attempt).
    pub retry_schedule_seconds: Vec<u64>,
    /// Total retry budget before promotion to permfail (hours).
    pub permanent_failure_timeout_hours: u32,
    /// Delay-warning emission time (hours).
    pub delay_warning_at_hours: u32,
    /// NDR per-recipient rate-limit window (days).
    pub ndr_rate_limit_days: u32,
    /// Suppress the NDR (backscatter) on an SPF hardfail.
    pub suppress_ndr_spf_hardfail: bool,
    /// Suppress the NDR on a DMARC reject/quarantine.
    pub suppress_ndr_dmarc_reject: bool,
    /// Postmaster CC on bounces — project policy is never CC (disabled in v1; the
    /// page renders this control read-only).
    pub postmaster_cc_bounces: bool,
    /// Emit TLSRPT outbound reports (cooperative behaviour, RFC 8460).
    pub tlsrpt_send_reports: bool,
    /// IPv6 outbound (auto-detected from interface; admin-overridable).
    pub ipv6_enabled: bool,
    /// Enhanced-status codes to treat as transient even when the wire is 5xx.
    pub treat_5xx_as_transient: Vec<String>,
}

impl From<OutboundPolicy> for OutboundPolicyView {
    fn from(p: OutboundPolicy) -> Self {
        Self {
            retry_schedule_seconds: p.retry_schedule_seconds,
            permanent_failure_timeout_hours: p.permanent_failure_timeout_hours,
            delay_warning_at_hours: p.delay_warning_at_hours,
            ndr_rate_limit_days: p.ndr_rate_limit_days,
            suppress_ndr_spf_hardfail: p.suppress_ndr_spf_hardfail,
            suppress_ndr_dmarc_reject: p.suppress_ndr_dmarc_reject,
            postmaster_cc_bounces: p.postmaster_cc_bounces,
            tlsrpt_send_reports: p.tlsrpt_send_reports,
            ipv6_enabled: p.ipv6_enabled,
            treat_5xx_as_transient: p.treat_5xx_as_transient,
        }
    }
}

impl OutboundPolicyView {
    fn into_put_request(self) -> PutOutboundPolicyRequest {
        PutOutboundPolicyRequest {
            retry_schedule_seconds: Some(self.retry_schedule_seconds),
            permanent_failure_timeout_hours: Some(self.permanent_failure_timeout_hours),
            delay_warning_at_hours: Some(self.delay_warning_at_hours),
            ndr_rate_limit_days: Some(self.ndr_rate_limit_days),
            suppress_ndr_spf_hardfail: Some(self.suppress_ndr_spf_hardfail),
            suppress_ndr_dmarc_reject: Some(self.suppress_ndr_dmarc_reject),
            postmaster_cc_bounces: Some(self.postmaster_cc_bounces),
            tlsrpt_send_reports: Some(self.tlsrpt_send_reports),
            ipv6_enabled: Some(self.ipv6_enabled),
            treat_5xx_as_transient: Some(self.treat_5xx_as_transient),
            // Forward-compat catch-all (transport.md rule 4); nothing to
            // carry forward on a locally-built request.
            extra: Default::default(),
        }
    }
}

/// The nest-side **alias** policy sub-struct as the `admin-mail` form renders +
/// edits it. Field-identical to the wire [`AliasPolicy`] (the effective
/// override-or-default policy). Unlike the five `*PolicyView`s above — which
/// project from `FetchConfigReply` (one `get_mail_config` read) — this projects
/// from the **separate** `get_alias_policy` admin twin, because the alias knobs
/// are not in `FetchConfigReply` (consumed nest-side by the resolver + alias
/// CRUD). `mail-policy-config.md` § Inbound perimeter (alias rows).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AliasPolicyView {
    /// Per-account cap on user-added exact aliases (default 20).
    pub exact_aliases_max: u32,
    /// Reserved local-part list — role addresses users cannot claim
    /// (full-replace on save; empty clears the reservation).
    pub reserved_local_parts: Vec<String>,
    /// `+suffix` sub-addressing enabled (default `true`).
    pub subaddressing_enabled: bool,
    /// `bob-*` wildcard-prefix aliases enabled (default `true`).
    pub wildcard_prefix_enabled: bool,
}

impl From<AliasPolicy> for AliasPolicyView {
    fn from(p: AliasPolicy) -> Self {
        Self {
            exact_aliases_max: p.exact_aliases_max,
            reserved_local_parts: p.reserved_local_parts,
            subaddressing_enabled: p.subaddressing_enabled,
            wildcard_prefix_enabled: p.wildcard_prefix_enabled,
        }
    }
}

impl AliasPolicyView {
    /// Full-PUT request: the form edits the whole sub-struct, so every field is
    /// sent `Some(_)` (a replace, not a merge — `PutAliasPolicyRequest` doc).
    fn into_put_request(self) -> PutAliasPolicyRequest {
        PutAliasPolicyRequest {
            exact_aliases_max: Some(self.exact_aliases_max),
            reserved_local_parts: Some(self.reserved_local_parts),
            subaddressing_enabled: Some(self.subaddressing_enabled),
            wildcard_prefix_enabled: Some(self.wildcard_prefix_enabled),
            // Forward-compat catch-all (transport.md rule 4); nothing to
            // carry forward on a locally-built request.
            extra: Default::default(),
        }
    }
}

/// The outcome of the last `PublishSpamBaseline` action, as the `admin-mail`
/// Spam group renders it. **Aggregate-only** — never a contributor identity
/// (`mail-spam.md` § Cold start Path 2, "the admin cannot view individual
/// contributions"). Projected once from [`PublishSpamBaselineReply`] so all
/// seven apps render off the same record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct BaselinePublishView {
    /// Whether a real (non-empty) baseline was published — the wire
    /// `published` flag, read as sent. `false` ⇒ the k-anonymity floor withheld (fewer than
    /// `fauna_mail::spam::BASELINE_MIN_CONTRIBUTORS` opt-in contributors); the
    /// UI surfaces "not published — too few contributors".
    pub published: bool,
    /// Opt-in users whose trained per-user model merged into the baseline.
    pub contributors: u32,
    /// Total training samples (`spam + ham`) behind the published baseline.
    pub sample_count: u32,
    /// Opt-in contributors whose sealed model could **not** be merged this run
    /// (holder unavailable, no reaching grant + copy, or an undecodable copy —
    /// `mail-spam.md` § Encrypted-mode interaction, the silent-erosion fix).
    /// Additive: absent from a pre-grant-plumbing nest ⇒ `0` (serde default),
    /// which renders no erosion note — correct, since such a nest has no
    /// holder-side drain to skip anyone from. Surfaced beside the
    /// published/withheld message so the admin sees the erosion instead of a
    /// quietly-shrunken contributor count.
    pub skipped_contributors: u32,
    /// The run was deferred by the delta floor (`mail-spam.md` § Cold start
    /// Path 2 → *The floor applies to every published DELTA*): nothing was
    /// written and the box keeps serving what it had. `published` is `false`
    /// here too, so a renderer checks this FIRST — the UI surfaces "waiting for
    /// more contributor activity", never "too few contributors". Absent on the wire ⇒ `false`
    /// (serde default).
    pub deferred: bool,
}

impl From<PublishSpamBaselineReply> for BaselinePublishView {
    fn from(r: PublishSpamBaselineReply) -> Self {
        Self {
            published: r.published,
            contributors: r.contributors,
            sample_count: r.sample_count,
            skipped_contributors: r.skipped_contributors,
            deferred: r.deferred,
        }
    }
}

/// The deployment spam baseline's CURRENT state, as the `admin-mail` Spam
/// group renders it: "published over N contributors on <date>" / "no baseline
/// published", either followed by "waiting for more contributor activity" when
/// the last run was deferred (`mail-spam.md` § Cold start Path 2 → *Standing
/// publish*). Projected from [`GetSpamBaselineStateReply`]. **Aggregate-only,
/// and history-free** — there is no withdrawal time or reason to render: a
/// withdrawn baseline reads as one never published.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct BaselineStateView {
    /// A real baseline is served now.
    pub published: bool,
    /// Contributors summed into the served baseline (`0` when none is served).
    pub contributors: u32,
    /// Training samples behind the served baseline (`0` when none is served).
    pub sample_count: u32,
    /// When the served baseline was built, epoch milliseconds (`None` when
    /// none is served).
    pub published_at_ms: Option<i64>,
    /// The last run's opted-in contributors it could not merge.
    pub skipped_contributors: u32,
    /// The last run was deferred by the delta floor — render "waiting for more
    /// contributor activity".
    pub deferred: bool,
    /// Standing publish is on.
    pub standing: bool,
}

impl From<GetSpamBaselineStateReply> for BaselineStateView {
    fn from(r: GetSpamBaselineStateReply) -> Self {
        Self {
            published: r.published,
            contributors: r.contributors,
            sample_count: r.sample_count,
            published_at_ms: r.published_at,
            skipped_contributors: r.skipped_contributors,
            deferred: r.deferred,
            standing: r.standing,
        }
    }
}

/// Coarse machine status for spinner / disabled-control rendering. Mirrors
/// `ForwarderStatus` / `LocalDomainStatus`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum MailPolicyStatus {
    Idle,
    Loading,
    Working,
}

/// One row of the mail health readout (`admin-mail-health-check`). `label_key`
/// is the row's i18n key; `state` (`pass` / `warn` / `fail` / `info`, open)
/// renders through `fauna_core::format::mail_health_check_state_label`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct MailHealthCheckView {
    pub label_key: String,
    pub state: String,
    pub detail: String,
}

impl From<MailHealthCheck> for MailHealthCheckView {
    fn from(c: MailHealthCheck) -> Self {
        Self {
            label_key: c.label_key,
            state: c.state,
            detail: c.detail,
        }
    }
}

/// The mail health readout at the top of `admin-mail` — `fauna.bridges.mail_health`
/// projected for the renderer (`mail-deliverability.md` § The mail health
/// readout). `state` is the open-enum categorical line, rendered through
/// `fauna_core::format::mail_health_state_label` (unknown → "needs attention");
/// `checks` are the seven rows in their fixed order; the two heartbeats are Unix
/// seconds (`None` = never) and render as facts; `delist_url` backs
/// `admin-mail-health-delist-link`, present only while the outbound IP is listed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct MailHealthView {
    pub state: String,
    pub checks: Vec<MailHealthCheckView>,
    pub last_outbound_delivered_at: Option<i64>,
    pub last_inbound_accepted_at: Option<i64>,
    pub delist_url: Option<String>,
}

impl From<MailHealthReply> for MailHealthView {
    fn from(r: MailHealthReply) -> Self {
        Self {
            state: r.state,
            checks: r.checks.into_iter().map(Into::into).collect(),
            last_outbound_delivered_at: r.last_outbound_delivered_at,
            last_inbound_accepted_at: r.last_inbound_accepted_at,
            delist_url: r.delist_url,
        }
    }
}

/// Read-only snapshot the per-app `admin-mail` UI renders. Projected from the
/// admin read twin `get_mail_config` (the overlaid effective config).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct MailPolicySnapshot {
    /// Deployment-wide mail-enable toggle (`mail.enabled` / `set_mail_enabled`).
    pub mail_enabled: bool,
    /// Deployment-wide "auto-enable mail for new users" policy
    /// (`set_auto_enable_mail_for_new_users`; default-**on**). Decides whether a
    /// freshly-registered user's client auto-provisions its own mailbox on first
    /// setup. Unlike `mail_enabled` this is **not** in `FetchConfigReply` (the
    /// bridge never reads it) — it hydrates from the *separate* `fauna.setup.status`
    /// read twin, exactly as the alias group hydrates from `get_alias_policy`.
    /// `mail-policy-config.md` § Tier-2 *Auto-enable mail for new users*.
    pub auto_enable_mail_for_new_users: bool,
    /// The Spam/Inbound-perimeter sub-struct (`put_spam_policy`).
    pub spam: SpamPolicyView,
    /// The Auth/enforcement sub-struct (`put_auth_policy`).
    pub auth: AuthPolicyView,
    /// The Submission-quota sub-struct (`put_submission_policy`).
    pub submission: SubmissionPolicyView,
    /// The IMAP-server policy sub-struct (`put_imap_policy`).
    pub imap: ImapPolicyView,
    /// The Outbound-delivery sub-struct (`put_outbound_policy`).
    pub outbound: OutboundPolicyView,
    /// The nest-side **alias** policy sub-struct (`put_alias_policy`). Hydrated
    /// from the *separate* `get_alias_policy` read twin (not `FetchConfigReply`).
    pub alias: AliasPolicyView,
    pub status: MailPolicyStatus,
    /// Outcome of the last `PublishSpamBaseline` action (`None` until the admin
    /// clicks the publish-baseline button). Rendered in the Spam group as the
    /// published/withheld status line. **Not** hydrated on `Refresh` — it is a
    /// transient action outcome, not persisted policy — so any subsequent
    /// re-read (a save, a toggle) clears it back to `None`.
    pub baseline_publish_result: Option<BaselinePublishView>,
    /// The deployment baseline's current state (`get_spam_baseline_state`),
    /// rendered as `admin-mail-spam-baseline-state`. Re-read on every refresh
    /// and after "publish now". `None` before the first hydrate — the renderer
    /// then paints the text empty rather than guess.
    pub baseline_state: Option<BaselineStateView>,
    /// The mail health readout (`fauna.bridges.mail_health`), re-read on every
    /// refresh and after a recheck or warm-up reset. `None` before the first
    /// hydrate — the renderer then omits the section rather than guess.
    pub health: Option<MailHealthView>,
    /// Last action's error, surfaced via `admin-mail`'s `error-message`
    /// (`fauna.protocol.malformed` on an out-of-order spam-threshold write, etc.).
    pub error: Option<String>,
}

impl MailPolicySnapshot {
    /// The catalog-default config (what a freshly-claimed nest reports). Used as
    /// the pre-hydrate placeholder so the snapshot is never in an invalid state.
    /// The auto-enable-for-new-users policy defaults **on** (the works-out-of-box
    /// invariant; unset ⇒ ON — `mail-policy-config.md` § Tier-2).
    fn defaults() -> Self {
        Self::from_config(FetchConfigReply::default(), AliasPolicy::default(), true)
    }

    /// Build the rendered snapshot from the three admin reads: the overlaid
    /// effective config (`get_mail_config`) for the five projected groups + the
    /// mail-enable toggle, the effective alias policy (`get_alias_policy`) for the
    /// alias group, and the new-user auto-enable policy (`fauna.setup.status`) —
    /// the latter two are not projected into `FetchConfigReply`.
    fn from_config(
        reply: FetchConfigReply,
        alias: AliasPolicy,
        auto_enable_mail_for_new_users: bool,
    ) -> Self {
        Self {
            mail_enabled: reply.mail_enabled,
            auto_enable_mail_for_new_users,
            spam: reply.spam.into(),
            auth: reply.auth.into(),
            submission: reply.submission.into(),
            imap: reply.imap.into(),
            outbound: reply.outbound.into(),
            alias: alias.into(),
            status: MailPolicyStatus::Idle,
            baseline_publish_result: None,
            baseline_state: None,
            health: None,
            error: None,
        }
    }
}

/// Actions the per-app `admin-mail` UI dispatches.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum MailPolicyAction {
    /// Re-read the effective config (page load / after a save).
    Refresh,
    /// Flip the deployment-wide mail-enable toggle.
    SetMailEnabled { enabled: bool },
    /// Flip the deployment-wide "auto-enable mail for new users" policy
    /// (`set_auto_enable_mail_for_new_users`). Persisted via `fauna.bridges`,
    /// re-read via `fauna.setup.status`.
    SetAutoEnableMailForNewUsers { enabled: bool },
    /// Persist the whole Spam/Inbound sub-struct (full PUT). nest rejects an
    /// out-of-order pair (`spam_folder < reject` when both are non-zero).
    SaveSpam { policy: SpamPolicyView },
    /// Persist the whole Auth sub-struct (full PUT).
    SaveAuth { policy: AuthPolicyView },
    /// Persist the whole Submission-quota sub-struct (full PUT).
    SaveSubmission { policy: SubmissionPolicyView },
    /// Persist the whole IMAP-server policy sub-struct (full PUT).
    SaveImap { policy: ImapPolicyView },
    /// Persist the whole Outbound-delivery sub-struct (full PUT).
    SaveOutbound { policy: OutboundPolicyView },
    /// Persist the whole nest-side alias-policy sub-struct (full PUT via
    /// `put_alias_policy`; re-read via `get_alias_policy`).
    SaveAlias { policy: AliasPolicyView },
    /// Publish the current aggregate of opt-in users' models as the deployment
    /// baseline (`fauna.bridges.publish_spam_baseline`). Admin-only, no payload
    /// (caller-scoped). Aggregate-only reply; the k-anonymity floor withholds
    /// below `BASELINE_MIN_CONTRIBUTORS` (`published = false`). The outcome
    /// lands in [`MailPolicySnapshot::baseline_publish_result`], not a re-read.
    /// `mail-spam.md` § Cold start Path 2.
    PublishSpamBaseline,
    /// Flip standing baseline publish (`admin-mail-spam-baseline-standing-toggle`)
    /// on its own, outside the Spam group's Save: full-PUTs the PERSISTED spam
    /// sub-struct with only this field changed, then re-reads. Off withdraws
    /// the served baseline nest-side. `mail-spam.md` § Cold start Path 2 →
    /// *Standing publish*.
    SetBaselineStandingPublish { enabled: bool },
    /// `admin-mail-health-recheck-button`: run a fresh blocklist self-check
    /// (`blocklist_self_check_run`) and deliverability diagnostics
    /// (`run_deliverability_diagnostics`), then re-read the health readout.
    RecheckHealth,
    /// `admin-mail-health-warmup-reset-button` (confirm-gated by the renderer):
    /// restart the fresh-IP warm-up ramp at day 1 (`outbound_warmup_reset`), the
    /// after-an-IP-change act, then re-read the health readout.
    ResetWarmup,
}

/// WS-RPC seam to nest. Per-app glue implements this over `MailAdminClient`
/// (`libs/fauna-client-bridges`) — each method a 1:1 forward. Dual `async_trait`
/// arm + `MaybeSendSync` supertrait so one seam serves native + wasm.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait MailPolicyNest: MaybeSendSync {
    /// `fauna.bridges.get_mail_config` — the overlaid effective config (the admin
    /// read twin of the bridge's `fetch_config`).
    async fn get_mail_config(&self) -> Result<FetchConfigReply, NestError>;
    /// `fauna.bridges.set_mail_enabled`.
    async fn set_mail_enabled(&self, enabled: bool) -> Result<(), NestError>;
    /// `fauna.setup.status` → `auto_enable_mail_for_new_users` — the read twin of
    /// the new-user auto-enable policy (not in `FetchConfigReply`).
    async fn get_auto_enable_mail_for_new_users(&self) -> Result<bool, NestError>;
    /// `fauna.bridges.set_auto_enable_mail_for_new_users`.
    async fn set_auto_enable_mail_for_new_users(&self, enabled: bool) -> Result<(), NestError>;
    /// `fauna.bridges.put_spam_policy`.
    async fn put_spam_policy(&self, req: PutSpamPolicyRequest) -> Result<(), NestError>;
    /// `fauna.bridges.put_auth_policy`.
    async fn put_auth_policy(&self, req: PutAuthPolicyRequest) -> Result<(), NestError>;
    /// `fauna.bridges.put_submission_policy`.
    async fn put_submission_policy(&self, req: PutSubmissionPolicyRequest)
    -> Result<(), NestError>;
    /// `fauna.bridges.put_imap_policy`.
    async fn put_imap_policy(&self, req: PutImapPolicyRequest) -> Result<(), NestError>;
    /// `fauna.bridges.put_outbound_policy`.
    async fn put_outbound_policy(&self, req: PutOutboundPolicyRequest) -> Result<(), NestError>;
    /// `fauna.bridges.get_alias_policy` — the effective nest-side alias policy
    /// (the alias group's read twin; not in `FetchConfigReply`).
    async fn get_alias_policy(&self) -> Result<AliasPolicy, NestError>;
    /// `fauna.bridges.put_alias_policy`.
    async fn put_alias_policy(&self, req: PutAliasPolicyRequest) -> Result<(), NestError>;
    /// `fauna.bridges.publish_spam_baseline` — aggregate opt-in users' trained
    /// models into the deployment baseline (admin-only). Aggregate-only reply
    /// (no contributor identity); withheld below the k-anonymity floor
    /// (`published = false`).
    async fn publish_spam_baseline(&self) -> Result<PublishSpamBaselineReply, NestError>;
    /// `fauna.bridges.get_spam_baseline_state` — the served baseline's current
    /// state (admin-only; aggregate-only and history-free).
    async fn get_spam_baseline_state(&self) -> Result<GetSpamBaselineStateReply, NestError>;
    /// `fauna.bridges.mail_health` — the mail health readout (admin-only).
    async fn mail_health(&self) -> Result<MailHealthReply, NestError>;
    /// `fauna.bridges.blocklist_self_check_run` — force-refresh the DNSBL set.
    async fn blocklist_self_check_run(&self) -> Result<(), NestError>;
    /// `fauna.bridges.run_deliverability_diagnostics` — run the checklist now.
    async fn run_deliverability_diagnostics(&self) -> Result<(), NestError>;
    /// `fauna.bridges.outbound_warmup_reset` — restart the warm-up at day 1.
    async fn outbound_warmup_reset(&self) -> Result<(), NestError>;
}

/// One instance per admin client. Holds the rendered snapshot; drives the seam.
/// Mirrors `ForwarderMachine` / `LocalDomainMachine`.
#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct MailPolicyMachine {
    nest: Arc<dyn MailPolicyNest>,
    inner: Mutex<MailPolicySnapshot>,
}

impl MailPolicyMachine {
    pub fn new(nest: Arc<dyn MailPolicyNest>) -> Self {
        Self {
            nest,
            inner: Mutex::new(MailPolicySnapshot::defaults()),
        }
    }

    fn set_status(&self, status: MailPolicyStatus) {
        self.inner.lock().expect("snapshot mutex").status = status;
    }

    async fn refresh(&self) -> Result<(), DispatchError> {
        self.set_status(MailPolicyStatus::Loading);
        // Three reads per refresh: `get_mail_config` for the five projected groups
        // + the mail-enable toggle, the separate `get_alias_policy` twin for the
        // alias group, and `fauna.setup.status` for the new-user auto-enable
        // policy — the latter two are not in `FetchConfigReply` (consumed
        // nest-side / client-read). mail-policy-config.md § Implementation status.
        let reply = self.nest.get_mail_config().await?;
        let alias = self.nest.get_alias_policy().await?;
        let auto_enable = self.nest.get_auto_enable_mail_for_new_users().await?;
        // A fourth read for the baseline state text, a fifth for the health
        // readout.
        let baseline_state = Some(self.nest.get_spam_baseline_state().await?.into());
        let health = Some(self.nest.mail_health().await?.into());
        let mut snap = self.inner.lock().expect("snapshot mutex");
        *snap = MailPolicySnapshot::from_config(reply, alias, auto_enable);
        snap.baseline_state = baseline_state;
        snap.health = health;
        snap.status = MailPolicyStatus::Idle;
        Ok(())
    }

    /// Re-read only the health readout into the snapshot (after an act that
    /// moves it; the policy form's drafts are left alone).
    async fn refresh_health(&self) -> Result<(), DispatchError> {
        let health = self.nest.mail_health().await?.into();
        let mut snap = self.inner.lock().expect("snapshot mutex");
        snap.health = Some(health);
        snap.status = MailPolicyStatus::Idle;
        Ok(())
    }

    async fn recheck_health(&self) -> Result<(), DispatchError> {
        self.set_status(MailPolicyStatus::Working);
        self.nest.blocklist_self_check_run().await?;
        self.nest.run_deliverability_diagnostics().await?;
        self.refresh_health().await
    }

    async fn reset_warmup(&self) -> Result<(), DispatchError> {
        self.set_status(MailPolicyStatus::Working);
        self.nest.outbound_warmup_reset().await?;
        self.refresh_health().await
    }

    async fn set_baseline_standing_publish(&self, enabled: bool) -> Result<(), DispatchError> {
        // The persisted group, not the form's drafts: the toggle is its own
        // gesture, and every other field must go back out as stored (an omitted
        // field would reset to the catalog default).
        let mut policy = fauna_core::clone_locked(&self.inner, |s| &s.spam);
        policy.baseline_standing_publish = enabled;
        self.save_spam(policy).await
    }

    async fn set_mail_enabled(&self, enabled: bool) -> Result<(), DispatchError> {
        self.set_status(MailPolicyStatus::Working);
        self.nest.set_mail_enabled(enabled).await?;
        // Re-read so the toggle reflects persisted state.
        self.refresh().await
    }

    async fn set_auto_enable_mail_for_new_users(&self, enabled: bool) -> Result<(), DispatchError> {
        self.set_status(MailPolicyStatus::Working);
        self.nest
            .set_auto_enable_mail_for_new_users(enabled)
            .await?;
        // Re-read so the toggle reflects persisted state (via setup.status).
        self.refresh().await
    }

    async fn save_spam(&self, policy: SpamPolicyView) -> Result<(), DispatchError> {
        self.set_status(MailPolicyStatus::Working);
        self.nest.put_spam_policy(policy.into_put_request()).await?;
        self.refresh().await
    }

    async fn save_auth(&self, policy: AuthPolicyView) -> Result<(), DispatchError> {
        self.set_status(MailPolicyStatus::Working);
        self.nest.put_auth_policy(policy.into_put_request()).await?;
        self.refresh().await
    }

    async fn save_submission(&self, policy: SubmissionPolicyView) -> Result<(), DispatchError> {
        self.set_status(MailPolicyStatus::Working);
        self.nest
            .put_submission_policy(policy.into_put_request())
            .await?;
        self.refresh().await
    }

    async fn save_imap(&self, policy: ImapPolicyView) -> Result<(), DispatchError> {
        self.set_status(MailPolicyStatus::Working);
        self.nest.put_imap_policy(policy.into_put_request()).await?;
        self.refresh().await
    }

    async fn save_outbound(&self, policy: OutboundPolicyView) -> Result<(), DispatchError> {
        self.set_status(MailPolicyStatus::Working);
        self.nest
            .put_outbound_policy(policy.into_put_request())
            .await?;
        self.refresh().await
    }

    async fn save_alias(&self, policy: AliasPolicyView) -> Result<(), DispatchError> {
        self.set_status(MailPolicyStatus::Working);
        self.nest
            .put_alias_policy(policy.into_put_request())
            .await?;
        self.refresh().await
    }

    async fn publish_spam_baseline(&self) -> Result<(), DispatchError> {
        self.set_status(MailPolicyStatus::Working);
        // The reply IS the outcome — there is nothing to re-read (the published
        // baseline is deployment state the admin page doesn't otherwise render),
        // so this does NOT call `refresh()` (which would reset the result to
        // `None`). It stashes the aggregate view directly, and re-reads only
        // the baseline state, which the run just moved.
        let reply = self.nest.publish_spam_baseline().await?;
        let baseline_state = Some(self.nest.get_spam_baseline_state().await?.into());
        let mut snap = self.inner.lock().expect("snapshot mutex");
        snap.baseline_publish_result = Some(reply.into());
        snap.baseline_state = baseline_state;
        snap.status = MailPolicyStatus::Idle;
        Ok(())
    }
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
impl MailPolicyMachine {
    pub fn snapshot(&self) -> MailPolicySnapshot {
        fauna_core::clone_locked(&self.inner, |s| s)
    }
}

#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl MailPolicyMachine {
    /// Initial page load — hydrate from `get_mail_config`.
    pub async fn hydrate(&self) -> Result<(), DispatchError> {
        self.refresh().await
    }

    pub async fn dispatch(&self, action: MailPolicyAction) -> Result<(), DispatchError> {
        // Clear any prior error before the new action runs.
        self.inner.lock().expect("snapshot mutex").error = None;
        crate::dispatch_capturing_error!(
            self,
            MailPolicyStatus,
            match action {
                MailPolicyAction::Refresh => self.refresh().await,
                MailPolicyAction::SetMailEnabled { enabled } => {
                    self.set_mail_enabled(enabled).await
                }
                MailPolicyAction::SetAutoEnableMailForNewUsers { enabled } => {
                    self.set_auto_enable_mail_for_new_users(enabled).await
                }
                MailPolicyAction::SaveSpam { policy } => self.save_spam(policy).await,
                MailPolicyAction::SaveAuth { policy } => self.save_auth(policy).await,
                MailPolicyAction::SaveSubmission { policy } => self.save_submission(policy).await,
                MailPolicyAction::SaveImap { policy } => self.save_imap(policy).await,
                MailPolicyAction::SaveOutbound { policy } => self.save_outbound(policy).await,
                MailPolicyAction::SaveAlias { policy } => self.save_alias(policy).await,
                MailPolicyAction::PublishSpamBaseline => self.publish_spam_baseline().await,
                MailPolicyAction::SetBaselineStandingPublish { enabled } => {
                    self.set_baseline_standing_publish(enabled).await
                }
                MailPolicyAction::RecheckHealth => self.recheck_health().await,
                MailPolicyAction::ResetWarmup => self.reset_warmup().await,
            }
        )
    }
}

// ── The two raw-value picker vocabularies of `admin-mail` ────────────────
//
// Both wire fields are `String`-typed on purpose (`bridge_routing.rs` — an
// unknown token from a newer nest must not fail to decode), which left the
// legal-value sets living in prose and every renderer inventing its own copy.
// Before 2026-08-23 each of the three vocabularies below was hand-written in
// `apps/fauna-tui/src/admin/mail.rs`, `apps/fauna-linux/src/settings/admin_mail.rs`
// and (for FCrDNS) again in Go's `internal/mta/policy.go` — with the *fallback*
// rule, which is a safety decision rather than a rendering detail, duplicated
// alongside them. Priority #1/#2/#4.

/// The `fcrdns_mode` picker's values (`admin-mail-fcrdns-mode-select`), in the
/// ratified render order.
///
/// Forward-confirmed rDNS mode: [`Off`](Self::Off) skips the check,
/// [`ScoreSignal`](Self::ScoreSignal) runs it and feeds the verdict to the spam
/// scorer without rejecting, [`Enforce`](Self::Enforce) rejects a failing
/// connection with `550 5.7.25` once `reject_fcrdns_fail` is also set. Catalog:
/// `mail-policy-config.md` § Inbound hardening.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FcrdnsMode {
    Off,
    ScoreSignal,
    Enforce,
}

impl FcrdnsMode {
    /// Render order — what every picker draws, and the index basis for a
    /// position-addressed widget (GTK `DropDown`, a terminal select).
    pub const ORDER: [FcrdnsMode; 3] = [Self::Off, Self::ScoreSignal, Self::Enforce];

    /// The wire token. Exhaustive by construction — never add a `_` arm.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::ScoreSignal => "score_signal",
            Self::Enforce => "enforce",
        }
    }

    /// The i18n key for this value's picker label.
    pub fn label_key(&self) -> &'static str {
        match self {
            Self::Off => "admin.mail_page.fcrdns_off",
            Self::ScoreSignal => "admin.mail_page.fcrdns_score_signal",
            Self::Enforce => "admin.mail_page.fcrdns_enforce",
        }
    }

    /// Read a wire token, falling back to [`ScoreSignal`](Self::ScoreSignal) for
    /// anything unrecognised — **the observability-first default**, and the same
    /// rule the Go MTA's `ParseFCrDNSMode` applies at the other end of the wire.
    ///
    /// The fallback is a policy decision, not a rendering one: silently choosing
    /// `Enforce` for a token we do not understand would start rejecting mail on a
    /// value nobody asked for, and choosing `Off` would silently disable a check
    /// the admin enabled. It belongs here, once, rather than in each renderer.
    pub fn from_wire_or_default(token: &str) -> Self {
        Self::ORDER
            .into_iter()
            .find(|m| m.as_str() == token)
            .unwrap_or(Self::ScoreSignal)
    }

    /// Index of a wire token in [`ORDER`](Self::ORDER) — what a
    /// position-addressed widget sets on hydrate. Unknown tokens land on the
    /// fallback's position, per [`from_wire_or_default`](Self::from_wire_or_default).
    pub fn index_of_wire(token: &str) -> usize {
        let m = Self::from_wire_or_default(token);
        Self::ORDER
            .into_iter()
            .position(|c| c == m)
            .expect("ORDER contains every variant")
    }
}

/// The IMAP `delete_nonempty` picker's values
/// (`admin-mail-imap-delete-nonempty-select`), in render order.
///
/// Whether an IMAP `DELETE` may remove a mailbox that still holds messages.
/// Catalog: `mail-policy-config.md`; the wire default is
/// [`Forbidden`](Self::Forbidden).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImapDeleteNonempty {
    Forbidden,
    Allowed,
}

impl ImapDeleteNonempty {
    /// Render order. `Forbidden` leads because it is the wire default and the
    /// safer of the two.
    pub const ORDER: [ImapDeleteNonempty; 2] = [Self::Forbidden, Self::Allowed];

    /// The wire token. Exhaustive by construction — never add a `_` arm.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Forbidden => "forbidden",
            Self::Allowed => "allowed",
        }
    }

    /// The i18n key for this value's picker label.
    pub fn label_key(&self) -> &'static str {
        match self {
            Self::Forbidden => "admin.mail_page.imap_delete_forbidden",
            Self::Allowed => "admin.mail_page.imap_delete_allowed",
        }
    }

    /// Read a wire token, falling back to [`Forbidden`](Self::Forbidden) — the
    /// wire default, and the one that cannot destroy a mailbox full of messages
    /// on a value we do not understand.
    pub fn from_wire_or_default(token: &str) -> Self {
        Self::ORDER
            .into_iter()
            .find(|m| m.as_str() == token)
            .unwrap_or(Self::Forbidden)
    }

    /// Index of a wire token in [`ORDER`](Self::ORDER).
    pub fn index_of_wire(token: &str) -> usize {
        let m = Self::from_wire_or_default(token);
        Self::ORDER
            .into_iter()
            .position(|c| c == m)
            .expect("ORDER contains every variant")
    }
}

/// The canonical `fcrdns_mode` picker options (wire value + i18n label key), in
/// render order — the same `Vec<ReachPolicyOption>` shape
/// [`fauna_core::format::unknown_sender_options`] and
/// `fauna_client_mail_settings::local_domains::role_address_options` use, so an
/// app that already resolves one catalog (via
/// [`fauna_core::format::resolve_option_labels`]) resolves this one unchanged.
/// Exported over UniFFI so android/apple/windows read this table instead of
/// hand-writing the value list (`mail-policy-config.md` § Architectural rules
/// → *An enumerated knob's value set has ONE owner*).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn fcrdns_mode_options() -> Vec<ReachPolicyOption> {
    FcrdnsMode::ORDER
        .into_iter()
        .map(|m| ReachPolicyOption {
            value: m.as_str().to_string(),
            label: LocalizedText::key(m.label_key()),
        })
        .collect()
}

/// The canonical IMAP `delete_nonempty` picker options, in render order.
/// Exported over UniFFI — see [`fcrdns_mode_options`].
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn imap_delete_nonempty_options() -> Vec<ReachPolicyOption> {
    ImapDeleteNonempty::ORDER
        .into_iter()
        .map(|m| ReachPolicyOption {
            value: m.as_str().to_string(),
            label: LocalizedText::key(m.label_key()),
        })
        .collect()
}

#[cfg(test)]
mod picker_vocabulary_tests {
    use super::*;

    /// The wire defaults these vocabularies must agree with, taken from the
    /// protocol crate's own `Default` impls rather than transcribed — the point
    /// of the exercise is that nobody restates the vocabulary, and a test that
    /// spells `"forbidden"` would be doing exactly that.
    #[test]
    fn the_fallbacks_match_the_wire_defaults_they_stand_in_for() {
        use fauna_protocol::bridge_routing::ImapPolicy;
        let wire_default = ImapPolicy::default().delete_nonempty;
        assert_eq!(
            ImapDeleteNonempty::from_wire_or_default("something-a-newer-nest-sent").as_str(),
            wire_default,
            "an unrecognised delete_nonempty must land on the wire default — anything \
             else silently changes whether IMAP DELETE can destroy a full mailbox"
        );
        // FCrDNS has no `Default` on its own sub-struct field to compare
        // against, so its fallback is asserted against the rule instead: it is
        // the observability-first middle, never the two ends.
        let fallback = FcrdnsMode::from_wire_or_default("something-a-newer-nest-sent");
        assert_eq!(fallback, FcrdnsMode::ScoreSignal);
        assert_ne!(
            fallback,
            FcrdnsMode::Enforce,
            "falling back to Enforce would start REJECTING mail on a token nobody chose"
        );
        assert_ne!(
            fallback,
            FcrdnsMode::Off,
            "falling back to Off would silently disable a check the admin enabled"
        );
    }

    #[test]
    fn every_variant_round_trips_and_indexes_where_the_order_says() {
        for (i, m) in FcrdnsMode::ORDER.into_iter().enumerate() {
            assert_eq!(FcrdnsMode::from_wire_or_default(m.as_str()), m);
            assert_eq!(FcrdnsMode::index_of_wire(m.as_str()), i);
        }
        for (i, m) in ImapDeleteNonempty::ORDER.into_iter().enumerate() {
            assert_eq!(ImapDeleteNonempty::from_wire_or_default(m.as_str()), m);
            assert_eq!(ImapDeleteNonempty::index_of_wire(m.as_str()), i);
        }
    }

    #[test]
    fn the_options_catalogs_are_the_order_with_distinct_values_and_labels() {
        for (name, opts, want) in [
            (
                "fcrdns_mode",
                fcrdns_mode_options(),
                FcrdnsMode::ORDER.len(),
            ),
            (
                "delete_nonempty",
                imap_delete_nonempty_options(),
                ImapDeleteNonempty::ORDER.len(),
            ),
        ] {
            assert_eq!(opts.len(), want, "{name}: catalog lost a value");
            let values: Vec<&str> = opts.iter().map(|o| o.value.as_str()).collect();
            let mut sorted = values.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(
                sorted.len(),
                values.len(),
                "{name}: two options share a wire value — the picker would write \
                 whichever the widget reached first"
            );
            let mut keys: Vec<String> = opts
                .iter()
                .map(|o| format!("{:?}", o.label))
                .collect::<Vec<_>>();
            keys.sort();
            keys.dedup();
            assert_eq!(
                keys.len(),
                opts.len(),
                "{name}: two options share a label — one value would be unpickable \
                 in any app that renders labels rather than raw tokens"
            );
        }
    }

    #[test]
    fn the_fcrdns_order_matches_the_ui_yaml_comment_it_renders() {
        // ui.yaml documents this select as "off / score_signal / enforce", and the
        // per-app pickers are position-addressed (a GTK DropDown index, a
        // terminal select index), so the ORDER is part of the cross-app contract
        // rather than a rendering preference. Pinned as a list, not per-index, so
        // the failure names the whole order.
        let order: Vec<&str> = FcrdnsMode::ORDER.into_iter().map(|m| m.as_str()).collect();
        assert_eq!(order, ["off", "score_signal", "enforce"]);
        let order: Vec<&str> = ImapDeleteNonempty::ORDER
            .into_iter()
            .map(|m| m.as_str())
            .collect();
        assert_eq!(order, ["forbidden", "allowed"]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// Applies each `Some(_)` field of `$req` onto `$target`, leaving `None`
    /// fields at their prior value — the one override-merge shape every
    /// `put_*_policy` FakeNest handler below needs (full-PUT of `Some` fields
    /// only, mirroring the real handlers' semantics).
    macro_rules! apply_overrides {
        ($target:expr, $req:expr, $($f:ident),+ $(,)?) => {
            $( if let Some(v) = $req.$f { $target.$f = v; } )+
        };
    }

    /// In-memory nest modelling the single-row policy override tables overlaid on
    /// catalog defaults: `get_mail_config` returns the current effective config;
    /// the put_* methods full-replace their sub-struct (and `put_spam_policy`
    /// enforces the non-zero-tier ordering, like the real handler); a config
    /// holds the persisted `FetchConfigReply`.
    struct FakeNest {
        cfg: StdMutex<FetchConfigReply>,
        /// The nest-side alias policy lives in its own single-row table (not in
        /// `FetchConfigReply`), so it is a separate cell read/written by the
        /// `get_alias_policy` / `put_alias_policy` seam methods.
        alias: StdMutex<AliasPolicy>,
        /// The new-user auto-enable policy singleton (unset ⇒ ON), surfaced on
        /// `fauna.setup.status` and written by `set_auto_enable_mail_for_new_users`
        /// — also not in `FetchConfigReply`.
        auto_enable: StdMutex<bool>,
        /// The aggregate `publish_spam_baseline` reports: `(contributors,
        /// sample_count)`. The stub mirrors the real handler's k-anonymity floor
        /// — it publishes (`published = true`) only at or above
        /// `BASELINE_MIN_CONTRIBUTORS` (= 3), else withholds. Default `(0, 0)`
        /// (nobody opted in ⇒ withheld).
        baseline_agg: StdMutex<(u32, u32)>,
        /// The `skipped_contributors` count the stub's `publish_spam_baseline`
        /// reply carries — independent of `baseline_agg`, since a real nest can
        /// both merge some contributors AND skip others in the same run. Default
        /// `0` (nobody eroded).
        baseline_skipped: StdMutex<u32>,
        /// Whether the stub's next `publish_spam_baseline` is deferred by the
        /// delta floor (nothing written, the served baseline kept).
        baseline_deferred: StdMutex<bool>,
        /// The served baseline's state `get_spam_baseline_state` reads. `None`
        /// makes the stub refuse the read. The stub
        /// keeps it in step with the other seams the way the real nest does: a
        /// publish that clears the floor serves a baseline, a deferred one only
        /// sets `deferred`, and `put_spam_policy` turning standing publish off
        /// withdraws the served baseline.
        baseline_state: StdMutex<Option<GetSpamBaselineStateReply>>,
        /// The `mail_health` readout. `None` makes the stub refuse the read.
        health: StdMutex<Option<MailHealthReply>>,
        /// The health acts the machine drove, in order.
        calls: StdMutex<Vec<&'static str>>,
    }

    /// The nest's hard-coded k-anonymity floor (`fauna_mail::spam::
    /// BASELINE_MIN_CONTRIBUTORS`); the FakeNest mirrors it so the machine tests
    /// exercise both the published and withheld branches.
    const FAKE_BASELINE_MIN_CONTRIBUTORS: u32 = 3;

    impl FakeNest {
        fn new() -> Self {
            Self {
                cfg: StdMutex::new(FetchConfigReply::default()),
                alias: StdMutex::new(AliasPolicy::default()),
                auto_enable: StdMutex::new(true),
                baseline_agg: StdMutex::new((0, 0)),
                baseline_skipped: StdMutex::new(0),
                baseline_deferred: StdMutex::new(false),
                baseline_state: StdMutex::new(Some(GetSpamBaselineStateReply::default())),
                health: StdMutex::new(Some(MailHealthReply {
                    state: "records_failing".into(),
                    checks: vec![MailHealthCheck {
                        label_key: "admin.mail_page.health_check_records".into(),
                        state: "fail".into(),
                        detail: "failing: SPF record valid".into(),
                        ..Default::default()
                    }],
                    last_outbound_delivered_at: Some(1_700_000_000),
                    ..Default::default()
                })),
                calls: StdMutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl MailPolicyNest for FakeNest {
        async fn get_mail_config(&self) -> Result<FetchConfigReply, NestError> {
            Ok(self.cfg.lock().unwrap().clone())
        }

        async fn set_mail_enabled(&self, enabled: bool) -> Result<(), NestError> {
            self.cfg.lock().unwrap().mail_enabled = enabled;
            Ok(())
        }

        async fn get_auto_enable_mail_for_new_users(&self) -> Result<bool, NestError> {
            Ok(*self.auto_enable.lock().unwrap())
        }

        async fn set_auto_enable_mail_for_new_users(&self, enabled: bool) -> Result<(), NestError> {
            *self.auto_enable.lock().unwrap() = enabled;
            Ok(())
        }

        async fn put_spam_policy(&self, req: PutSpamPolicyRequest) -> Result<(), NestError> {
            // Mirror the real handler's effective-ordering guard among the
            // non-zero tiers (0 = disabled, skipped).
            let mut cur = self.cfg.lock().unwrap();
            let s = &mut cur.spam;
            apply_overrides!(
                s,
                req,
                max_score_before_spam_folder,
                max_score_before_reject,
                dnsbl_servers,
                reject_no_rdns,
                greylist_enabled,
                greylist_delay_secs,
                max_conn_per_min,
                fcrdns_mode,
                helo_identity_required,
                reject_fcrdns_fail,
                max_message_bytes,
                bayesian_weight_milli,
                bayesian_min_samples,
                bayesian_full_confidence_samples,
                training_history_retention_days,
                unlisted_recipient_penalty
            );
            // Turning it off withdraws the served baseline.
            {
                let on = req.baseline_standing_publish;
                s.baseline_standing_publish = on;
                if let Some(state) = self.baseline_state.lock().unwrap().as_mut() {
                    state.standing = on;
                    if !on {
                        *state = GetSpamBaselineStateReply {
                            deferred: state.deferred,
                            ..Default::default()
                        };
                    }
                }
            }
            // Mirror the real handler's collapsed-confidence-ramp guard: an
            // effective full-confidence count at or below the min-samples floor
            // is a degenerate ramp.
            if s.bayesian_full_confidence_samples <= s.bayesian_min_samples {
                return Err(NestError::Rejected("fauna.protocol.malformed".into()));
            }
            let mut last = 0u32;
            let ordered = [s.max_score_before_spam_folder, s.max_score_before_reject]
                .into_iter()
                .all(|t| {
                    if t == 0 {
                        return true;
                    }
                    let ok = t > last;
                    last = t;
                    ok
                });
            if !ordered {
                return Err(NestError::Rejected("fauna.protocol.malformed".into()));
            }
            Ok(())
        }

        async fn put_auth_policy(&self, req: PutAuthPolicyRequest) -> Result<(), NestError> {
            let mut cur = self.cfg.lock().unwrap();
            let a = &mut cur.auth;
            apply_overrides!(
                a,
                req,
                enforce_dmarc,
                enforce_dkim,
                max_auth_failures_per_minute,
                max_conn_per_ip
            );
            Ok(())
        }

        async fn put_submission_policy(
            &self,
            req: PutSubmissionPolicyRequest,
        ) -> Result<(), NestError> {
            let mut cur = self.cfg.lock().unwrap();
            let s = &mut cur.submission;
            apply_overrides!(s, req, max_per_day, max_recipients_per_message);
            Ok(())
        }

        async fn put_imap_policy(&self, req: PutImapPolicyRequest) -> Result<(), NestError> {
            let mut cur = self.cfg.lock().unwrap();
            let i = &mut cur.imap;
            apply_overrides!(
                i,
                req,
                idle_timeout_secs,
                tombstone_retention_days,
                delete_nonempty,
                bodystructure_cache_max,
                storage_bytes_default,
                message_count_default,
            );
            Ok(())
        }

        async fn put_outbound_policy(
            &self,
            req: PutOutboundPolicyRequest,
        ) -> Result<(), NestError> {
            let mut cur = self.cfg.lock().unwrap();
            let o = &mut cur.outbound;
            apply_overrides!(
                o,
                req,
                retry_schedule_seconds,
                permanent_failure_timeout_hours,
                delay_warning_at_hours,
                ndr_rate_limit_days,
                suppress_ndr_spf_hardfail,
                suppress_ndr_dmarc_reject,
                postmaster_cc_bounces,
                tlsrpt_send_reports,
                ipv6_enabled,
                treat_5xx_as_transient,
            );
            Ok(())
        }

        async fn get_alias_policy(&self) -> Result<AliasPolicy, NestError> {
            Ok(self.alias.lock().unwrap().clone())
        }

        async fn put_alias_policy(&self, req: PutAliasPolicyRequest) -> Result<(), NestError> {
            // Full-PUT each Some(_) field onto the stored effective policy
            // (mirrors the real handler: None ⇒ keep prior/catalog default).
            let mut a = self.alias.lock().unwrap();
            apply_overrides!(
                a,
                req,
                exact_aliases_max,
                reserved_local_parts,
                subaddressing_enabled,
                wildcard_prefix_enabled
            );
            Ok(())
        }

        async fn publish_spam_baseline(&self) -> Result<PublishSpamBaselineReply, NestError> {
            let (contributors, agg_samples) = *self.baseline_agg.lock().unwrap();
            // Mirror the real handler's k-anonymity floor: withhold below
            // BASELINE_MIN_CONTRIBUTORS by publishing an EMPTY baseline
            // (`published = false`, `sample_count = 0`) — the real handler sets
            // `baseline = SpamModel::new()` below the floor, so the withheld
            // reply carries zero samples.
            let deferred = *self.baseline_deferred.lock().unwrap();
            let published = !deferred && contributors >= FAKE_BASELINE_MIN_CONTRIBUTORS;
            let skipped = *self.baseline_skipped.lock().unwrap();
            if let Some(state) = self.baseline_state.lock().unwrap().as_mut() {
                state.deferred = deferred;
                state.skipped_contributors = skipped;
                if published {
                    state.published = true;
                    state.contributors = contributors;
                    state.sample_count = agg_samples;
                    state.published_at = Some(1_758_000_000_000);
                }
            }
            Ok(PublishSpamBaselineReply {
                contributors,
                sample_count: if published { agg_samples } else { 0 },
                published,
                skipped_contributors: skipped,
                deferred,
                extra: Default::default(),
            })
        }

        async fn get_spam_baseline_state(&self) -> Result<GetSpamBaselineStateReply, NestError> {
            self.baseline_state
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| NestError::Rejected("fauna.rpc.method_not_found".into()))
        }

        async fn mail_health(&self) -> Result<MailHealthReply, NestError> {
            self.health
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| NestError::Rejected("fauna.rpc.method_not_found".into()))
        }

        async fn blocklist_self_check_run(&self) -> Result<(), NestError> {
            self.calls.lock().unwrap().push("blocklist_self_check_run");
            Ok(())
        }

        async fn run_deliverability_diagnostics(&self) -> Result<(), NestError> {
            self.calls
                .lock()
                .unwrap()
                .push("run_deliverability_diagnostics");
            // A fresh run clears the failing records in this stub.
            if let Some(h) = self.health.lock().unwrap().as_mut() {
                h.state = "delivering".into();
            }
            Ok(())
        }

        async fn outbound_warmup_reset(&self) -> Result<(), NestError> {
            self.calls.lock().unwrap().push("outbound_warmup_reset");
            if let Some(h) = self.health.lock().unwrap().as_mut() {
                h.state = "warming_up".into();
            }
            Ok(())
        }
    }

    fn machine() -> MailPolicyMachine {
        MailPolicyMachine::new(Arc::new(FakeNest::new()))
    }

    #[tokio::test]
    async fn hydrate_reads_the_health_readout() {
        let m = machine();
        assert_eq!(m.snapshot().health, None, "no readout before hydrate");
        m.hydrate().await.unwrap();
        let health = m.snapshot().health.expect("hydrated");
        assert_eq!(health.state, "records_failing");
        assert_eq!(health.checks.len(), 1);
        assert_eq!(health.last_outbound_delivered_at, Some(1_700_000_000));
    }

    #[tokio::test]
    async fn a_refused_mail_health_read_is_a_surfaced_error() {
        let nest = Arc::new(FakeNest::new());
        *nest.health.lock().unwrap() = None;
        let m = MailPolicyMachine::new(nest);
        m.dispatch(MailPolicyAction::Refresh).await.unwrap_err();
        let snap = m.snapshot();
        assert_eq!(snap.health, None);
        assert!(snap.error.is_some());
    }

    #[tokio::test]
    async fn recheck_runs_both_checks_then_rereads() {
        let nest = Arc::new(FakeNest::new());
        let m = MailPolicyMachine::new(nest.clone());
        m.hydrate().await.unwrap();
        m.dispatch(MailPolicyAction::RecheckHealth).await.unwrap();
        assert_eq!(
            *nest.calls.lock().unwrap(),
            vec!["blocklist_self_check_run", "run_deliverability_diagnostics"]
        );
        let snap = m.snapshot();
        assert_eq!(snap.health.expect("re-read").state, "delivering");
        assert_eq!(snap.status, MailPolicyStatus::Idle);
    }

    #[tokio::test]
    async fn warmup_reset_resets_then_rereads() {
        let nest = Arc::new(FakeNest::new());
        let m = MailPolicyMachine::new(nest.clone());
        m.hydrate().await.unwrap();
        m.dispatch(MailPolicyAction::ResetWarmup).await.unwrap();
        assert_eq!(*nest.calls.lock().unwrap(), vec!["outbound_warmup_reset"]);
        assert_eq!(m.snapshot().health.expect("re-read").state, "warming_up");
    }

    #[test]
    fn pre_hydrate_snapshot_is_catalog_default() {
        let m = machine();
        let snap = m.snapshot();
        // The machine starts on the catalog defaults (never an invalid state).
        assert_eq!(snap.spam.max_score_before_spam_folder, 5);
        assert!(snap.auth.enforce_dmarc);
        assert!(!snap.auth.enforce_dkim);
        // The alias group's pre-hydrate placeholder is the catalog default too.
        assert_eq!(snap.alias.exact_aliases_max, 20);
        assert_eq!(snap.alias.reserved_local_parts.len(), 6);
        assert!(snap.alias.subaddressing_enabled);
        assert!(snap.alias.wildcard_prefix_enabled);
        assert_eq!(snap.status, MailPolicyStatus::Idle);
    }

    #[tokio::test]
    async fn hydrate_projects_effective_config() {
        let m = machine();
        m.hydrate().await.unwrap();
        let snap = m.snapshot();
        assert_eq!(
            snap.spam.fcrdns_mode, "score_signal",
            "the spam sub-struct projects from get_mail_config"
        );
        assert_eq!(
            snap.spam.dnsbl_servers,
            vec!["zen.spamhaus.org".to_string()]
        );
        assert_eq!(snap.auth.max_auth_failures_per_minute, 30);
        assert!(snap.error.is_none());
    }

    #[tokio::test]
    async fn set_mail_enabled_persists_and_rereads() {
        let m = machine();
        m.hydrate().await.unwrap();
        assert!(m.snapshot().mail_enabled, "default config is enabled");
        m.dispatch(MailPolicyAction::SetMailEnabled { enabled: false })
            .await
            .unwrap();
        assert!(
            !m.snapshot().mail_enabled,
            "the toggle reflects the re-read persisted state"
        );
    }

    #[test]
    fn pre_hydrate_auto_enable_for_new_users_is_default_on() {
        // The pre-hydrate placeholder defaults ON (unset ⇒ ON, the works-out-of-
        // box invariant) — never an invalid/false placeholder.
        let m = machine();
        assert!(m.snapshot().auto_enable_mail_for_new_users);
    }

    #[tokio::test]
    async fn hydrate_projects_auto_enable_for_new_users_via_setup_status() {
        // The auto-enable policy hydrates from `fauna.setup.status`, NOT
        // `get_mail_config` — the third read must populate the snapshot flag.
        let m = MailPolicyMachine::new({
            let nest = FakeNest::new();
            *nest.auto_enable.lock().unwrap() = false;
            Arc::new(nest)
        });
        m.hydrate().await.unwrap();
        assert!(
            !m.snapshot().auto_enable_mail_for_new_users,
            "the snapshot reflects the setup.status flag"
        );
        assert!(m.snapshot().error.is_none());
    }

    #[tokio::test]
    async fn set_auto_enable_for_new_users_persists_and_rereads() {
        let m = machine();
        m.hydrate().await.unwrap();
        assert!(m.snapshot().auto_enable_mail_for_new_users, "default is ON");
        m.dispatch(MailPolicyAction::SetAutoEnableMailForNewUsers { enabled: false })
            .await
            .unwrap();
        assert!(
            !m.snapshot().auto_enable_mail_for_new_users,
            "the toggle reflects the re-read persisted state"
        );
        m.dispatch(MailPolicyAction::SetAutoEnableMailForNewUsers { enabled: true })
            .await
            .unwrap();
        assert!(m.snapshot().auto_enable_mail_for_new_users, "flips back ON");
    }

    #[tokio::test]
    async fn hydrate_projects_bayesian_knob_defaults() {
        // The four Tier-2 per-user knobs project through get_mail_config at their
        // catalog defaults (700 = 0.7 weight, 50/200 ramp, 30d retention).
        let m = machine();
        m.hydrate().await.unwrap();
        let s = m.snapshot().spam;
        assert_eq!(s.bayesian_weight_milli, 700);
        assert_eq!(s.bayesian_min_samples, 50);
        assert_eq!(s.bayesian_full_confidence_samples, 200);
        assert_eq!(s.training_history_retention_days, 30);
    }

    #[tokio::test]
    async fn save_spam_full_put_persists_and_rereads() {
        let m = machine();
        m.hydrate().await.unwrap();
        let mut spam = m.snapshot().spam;
        spam.max_score_before_spam_folder = 6;
        spam.fcrdns_mode = "enforce".into();
        // The four Tier-2 knobs full-PUT alongside the perimeter fields.
        spam.bayesian_weight_milli = 500;
        spam.bayesian_min_samples = 40;
        spam.bayesian_full_confidence_samples = 150;
        spam.training_history_retention_days = 14;
        m.dispatch(MailPolicyAction::SaveSpam { policy: spam })
            .await
            .unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.spam.max_score_before_spam_folder, 6);
        assert_eq!(snap.spam.fcrdns_mode, "enforce");
        assert_eq!(snap.spam.bayesian_weight_milli, 500);
        assert_eq!(snap.spam.bayesian_min_samples, 40);
        assert_eq!(snap.spam.bayesian_full_confidence_samples, 150);
        assert_eq!(snap.spam.training_history_retention_days, 14);
        assert!(snap.error.is_none());
    }

    #[tokio::test]
    async fn save_spam_preserves_unlisted_penalty_no_clobber() {
        // Regression guard for the full-PUT clobber: set the penalty, then save
        // an UNRELATED spam knob — the penalty must survive the replace-PUT.
        let m = machine();
        m.hydrate().await.unwrap();
        let mut spam = m.snapshot().spam;
        spam.unlisted_recipient_penalty = 1000;
        m.dispatch(MailPolicyAction::SaveSpam { policy: spam })
            .await
            .unwrap();
        assert_eq!(m.snapshot().spam.unlisted_recipient_penalty, 1000);

        // Now edit a different knob and save; the penalty must NOT reset to 0.
        let mut spam = m.snapshot().spam;
        spam.max_message_bytes = 42_000_000;
        m.dispatch(MailPolicyAction::SaveSpam { policy: spam })
            .await
            .unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.spam.max_message_bytes, 42_000_000);
        assert_eq!(
            snap.spam.unlisted_recipient_penalty, 1000,
            "full-PUT must not clobber the admin-set penalty"
        );
    }

    #[tokio::test]
    async fn save_spam_inverted_confidence_ramp_surfaces_error_and_keeps_state() {
        // full_confidence <= min_samples is a collapsed ramp — nest rejects with
        // `fauna.protocol.malformed`; the machine surfaces it and keeps state.
        let m = machine();
        m.hydrate().await.unwrap();
        let mut spam = m.snapshot().spam;
        spam.bayesian_min_samples = 200;
        spam.bayesian_full_confidence_samples = 150; // below the floor → inverted
        let err = m
            .dispatch(MailPolicyAction::SaveSpam { policy: spam })
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchError::Nest(_)), "got {err:?}");
        let snap = m.snapshot();
        assert!(
            snap.error.as_deref().unwrap().contains("malformed"),
            "error: {:?}",
            snap.error
        );
        // The rejected write left the persisted ramp at its defaults.
        assert_eq!(snap.spam.bayesian_min_samples, 50);
        assert_eq!(snap.spam.bayesian_full_confidence_samples, 200);
        assert_eq!(snap.status, MailPolicyStatus::Idle);
    }

    #[tokio::test]
    async fn save_spam_out_of_order_surfaces_error_and_keeps_state() {
        let m = machine();
        m.hydrate().await.unwrap();
        let mut spam = m.snapshot().spam;
        // reject (3) below spam_folder (5), both non-zero → refused.
        spam.max_score_before_reject = 3;
        let err = m
            .dispatch(MailPolicyAction::SaveSpam { policy: spam })
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchError::Nest(_)), "got {err:?}");
        let snap = m.snapshot();
        assert!(
            snap.error.as_deref().unwrap().contains("malformed"),
            "error: {:?}",
            snap.error
        );
        assert_eq!(
            snap.spam.max_score_before_reject, 0,
            "the rejected write left persisted state untouched"
        );
        assert_eq!(snap.status, MailPolicyStatus::Idle);
    }

    #[tokio::test]
    async fn save_auth_full_put_persists() {
        let m = machine();
        m.hydrate().await.unwrap();
        let mut auth = m.snapshot().auth;
        auth.enforce_dkim = true;
        auth.max_auth_failures_per_minute = 10;
        m.dispatch(MailPolicyAction::SaveAuth { policy: auth })
            .await
            .unwrap();
        let snap = m.snapshot();
        assert!(snap.auth.enforce_dkim);
        assert_eq!(snap.auth.max_auth_failures_per_minute, 10);
    }

    #[tokio::test]
    async fn hydrate_projects_submission_imap_outbound_substructs() {
        let m = machine();
        m.hydrate().await.unwrap();
        let snap = m.snapshot();
        // Submission catalog defaults.
        assert_eq!(snap.submission.max_per_day, 1000);
        assert_eq!(snap.submission.max_recipients_per_message, 100);
        // IMAP catalog defaults (incl. the String + u64 archetypes).
        assert_eq!(snap.imap.idle_timeout_secs, 1740);
        assert_eq!(snap.imap.delete_nonempty, "forbidden");
        assert_eq!(snap.imap.storage_bytes_default, 1 << 30);
        assert_eq!(snap.imap.message_count_default, 50_000);
        // Outbound catalog defaults (incl. the Vec<u64> + Vec<String> archetypes).
        assert_eq!(snap.outbound.retry_schedule_seconds.len(), 10);
        assert_eq!(snap.outbound.retry_schedule_seconds[1], 300);
        assert_eq!(snap.outbound.permanent_failure_timeout_hours, 120);
        assert!(snap.outbound.suppress_ndr_spf_hardfail);
        assert!(!snap.outbound.postmaster_cc_bounces);
        assert!(snap.outbound.treat_5xx_as_transient.is_empty());
    }

    #[tokio::test]
    async fn save_submission_full_put_persists_and_rereads() {
        let m = machine();
        m.hydrate().await.unwrap();
        let mut sub = m.snapshot().submission;
        sub.max_per_day = 500;
        sub.max_recipients_per_message = 50;
        m.dispatch(MailPolicyAction::SaveSubmission { policy: sub })
            .await
            .unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.submission.max_per_day, 500);
        assert_eq!(snap.submission.max_recipients_per_message, 50);
        assert!(snap.error.is_none());
    }

    #[tokio::test]
    async fn save_imap_full_put_persists_string_and_u64_fields() {
        let m = machine();
        m.hydrate().await.unwrap();
        let mut imap = m.snapshot().imap;
        imap.delete_nonempty = "allowed".into();
        imap.storage_bytes_default = 2 << 30;
        imap.idle_timeout_secs = 600;
        m.dispatch(MailPolicyAction::SaveImap { policy: imap })
            .await
            .unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.imap.delete_nonempty, "allowed");
        assert_eq!(snap.imap.storage_bytes_default, 2 << 30);
        assert_eq!(snap.imap.idle_timeout_secs, 600);
    }

    #[tokio::test]
    async fn hydrate_projects_alias_policy_via_separate_read() {
        // The alias group hydrates from `get_alias_policy`, NOT `get_mail_config`
        // — the dual-read refactor must populate `snapshot().alias`.
        let m = machine();
        m.hydrate().await.unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.alias.exact_aliases_max, 20);
        assert_eq!(snap.alias.reserved_local_parts.len(), 6);
        assert!(snap.alias.subaddressing_enabled);
        assert!(snap.alias.wildcard_prefix_enabled);
        assert!(snap.error.is_none());
    }

    #[tokio::test]
    async fn save_alias_full_put_persists_and_rereads() {
        let m = machine();
        m.hydrate().await.unwrap();
        let mut alias = m.snapshot().alias;
        alias.exact_aliases_max = 3;
        alias.subaddressing_enabled = false;
        alias.reserved_local_parts = vec!["postmaster".into(), "sales".into()];
        m.dispatch(MailPolicyAction::SaveAlias { policy: alias })
            .await
            .unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.alias.exact_aliases_max, 3);
        assert!(!snap.alias.subaddressing_enabled);
        assert_eq!(
            snap.alias.reserved_local_parts,
            vec!["postmaster".to_string(), "sales".to_string()]
        );
        // The unmentioned wildcard toggle keeps its persisted value.
        assert!(snap.alias.wildcard_prefix_enabled);
        assert!(snap.error.is_none());
    }

    #[tokio::test]
    async fn save_outbound_full_put_persists_list_and_bool_fields() {
        let m = machine();
        m.hydrate().await.unwrap();
        let mut out = m.snapshot().outbound;
        out.retry_schedule_seconds = vec![0, 60, 600];
        out.suppress_ndr_dmarc_reject = false;
        out.treat_5xx_as_transient = vec!["4.2.2".into(), "5.7.1".into()];
        m.dispatch(MailPolicyAction::SaveOutbound { policy: out })
            .await
            .unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.outbound.retry_schedule_seconds, vec![0, 60, 600]);
        assert!(!snap.outbound.suppress_ndr_dmarc_reject);
        assert_eq!(
            snap.outbound.treat_5xx_as_transient,
            vec!["4.2.2".to_string(), "5.7.1".to_string()]
        );
    }

    #[test]
    fn pre_hydrate_baseline_publish_result_is_none() {
        // The publish-baseline result is a transient action outcome — never
        // hydrated — so the pre-hydrate placeholder carries none.
        let m = machine();
        assert!(m.snapshot().baseline_publish_result.is_none());
    }

    #[tokio::test]
    async fn publish_spam_baseline_above_floor_reports_published() {
        // ≥ 3 opt-in contributors → the nest publishes; the aggregate view lands
        // in the snapshot (published, with the contributor + sample counts).
        let m = MailPolicyMachine::new({
            let nest = FakeNest::new();
            *nest.baseline_agg.lock().unwrap() = (3, 42);
            Arc::new(nest)
        });
        m.hydrate().await.unwrap();
        m.dispatch(MailPolicyAction::PublishSpamBaseline)
            .await
            .unwrap();
        let r = m.snapshot().baseline_publish_result.unwrap();
        assert!(r.published);
        assert_eq!(r.contributors, 3);
        assert_eq!(r.sample_count, 42);
        assert!(m.snapshot().error.is_none());
        assert_eq!(m.snapshot().status, MailPolicyStatus::Idle);
    }

    #[tokio::test]
    async fn publish_spam_baseline_below_floor_reports_withheld() {
        // < 3 opt-in contributors → the k-anonymity floor withholds; the view
        // surfaces published = false with the (small) contributor count so the
        // UI can render "not published — too few contributors (N)".
        let m = MailPolicyMachine::new({
            let nest = FakeNest::new();
            *nest.baseline_agg.lock().unwrap() = (2, 20);
            Arc::new(nest)
        });
        m.hydrate().await.unwrap();
        m.dispatch(MailPolicyAction::PublishSpamBaseline)
            .await
            .unwrap();
        let r = m.snapshot().baseline_publish_result.unwrap();
        assert!(!r.published);
        assert_eq!(r.contributors, 2);
        assert!(m.snapshot().error.is_none());
    }

    #[tokio::test]
    async fn publish_spam_baseline_surfaces_skipped_contributors() {
        // The holder-side erosion count (mail-spam.md § Encrypted-mode
        // interaction, the silent-erosion fix) rides independently of the merged
        // count: 3 merged meets the floor and publishes, but 2 opted-in
        // contributors' sealed copies could not be merged this run — the view
        // must carry both so the UI can surface the erosion beside "published".
        let m = MailPolicyMachine::new({
            let nest = FakeNest::new();
            *nest.baseline_agg.lock().unwrap() = (3, 42);
            *nest.baseline_skipped.lock().unwrap() = 2;
            Arc::new(nest)
        });
        m.hydrate().await.unwrap();
        m.dispatch(MailPolicyAction::PublishSpamBaseline)
            .await
            .unwrap();
        let r = m.snapshot().baseline_publish_result.unwrap();
        assert!(r.published);
        assert_eq!(r.contributors, 3);
        assert_eq!(r.skipped_contributors, 2);
    }

    #[test]
    fn baseline_state_view_projects_the_reply_and_nothing_else() {
        let view: BaselineStateView = GetSpamBaselineStateReply {
            published: true,
            contributors: 4,
            sample_count: 90,
            published_at: Some(1_758_000_000_000),
            skipped_contributors: 1,
            deferred: true,
            standing: true,
            extra: Default::default(),
        }
        .into();
        assert_eq!(
            view,
            BaselineStateView {
                published: true,
                contributors: 4,
                sample_count: 90,
                published_at_ms: Some(1_758_000_000_000),
                skipped_contributors: 1,
                deferred: true,
                standing: true,
            }
        );
    }

    #[test]
    fn standing_publish_rides_the_full_put_unchanged() {
        // A form that does not render the toggle yet must not clobber it: the
        // hydrated value goes back out on every save (a `false` over a stored
        // `true` would withdraw the deployment's baseline).
        let mut spam = SpamPolicyView::from(SpamPolicyThresholds::default());
        assert!(!spam.baseline_standing_publish, "default off");
        spam.baseline_standing_publish = true;
        assert!(spam.into_put_request().baseline_standing_publish);
    }

    #[test]
    fn baseline_view_reads_the_published_flag_as_sent() {
        // The view reads the wire `published` flag directly — counts never
        // imply a publish (no reader fold for a nest that omitted the flag).
        let reply = PublishSpamBaselineReply {
            contributors: 5,
            sample_count: 100,
            published: false,
            skipped_contributors: 0,
            deferred: false,
            extra: Default::default(),
        };
        let view: BaselinePublishView = reply.into();
        assert!(!view.published, "published is the wire flag, never derived");
        assert_eq!(view.contributors, 5);
        assert_eq!(view.sample_count, 100);
    }

    #[tokio::test]
    async fn hydrate_reads_the_baseline_state() {
        // The state text renders off a read, not off the last click: a page
        // opened on a nest serving a baseline shows it before any action.
        let m = MailPolicyMachine::new({
            let nest = FakeNest::new();
            *nest.baseline_state.lock().unwrap() = Some(GetSpamBaselineStateReply {
                published: true,
                contributors: 4,
                sample_count: 90,
                published_at: Some(1_758_000_000_000),
                standing: true,
                ..Default::default()
            });
            Arc::new(nest)
        });
        assert!(m.snapshot().baseline_state.is_none(), "pre-hydrate");
        m.hydrate().await.unwrap();
        let state = m.snapshot().baseline_state.unwrap();
        assert!(state.published);
        assert_eq!(state.contributors, 4);
        assert_eq!(state.published_at_ms, Some(1_758_000_000_000));
        assert!(state.standing);
    }

    #[tokio::test]
    async fn a_refused_baseline_state_read_is_a_surfaced_error() {
        // Every nest serves the read, so a refusal is an ordinary error the
        // page surfaces like any other failed refresh.
        let m = MailPolicyMachine::new({
            let nest = FakeNest::new();
            *nest.baseline_state.lock().unwrap() = None;
            Arc::new(nest)
        });
        m.dispatch(MailPolicyAction::Refresh).await.unwrap_err();
        let snap = m.snapshot();
        assert!(snap.baseline_state.is_none());
        assert!(snap.error.is_some());
    }

    #[tokio::test]
    async fn standing_toggle_writes_only_its_field_and_rereads() {
        // The toggle dispatches on its own: it full-PUTs the PERSISTED spam
        // group with the one field flipped, so a persisted threshold survives
        // (a `None` would reset it to the catalog default) and the re-read
        // flips both the toggle and the state's `standing`.
        let nest = Arc::new(FakeNest::new());
        nest.cfg.lock().unwrap().spam.max_score_before_spam_folder = 4;
        let m = MailPolicyMachine::new(nest.clone());
        m.hydrate().await.unwrap();
        m.dispatch(MailPolicyAction::SetBaselineStandingPublish { enabled: true })
            .await
            .unwrap();
        let snap = m.snapshot();
        assert!(snap.spam.baseline_standing_publish);
        assert_eq!(snap.spam.max_score_before_spam_folder, 4, "no clobber");
        assert!(snap.baseline_state.unwrap().standing);
        assert!(snap.error.is_none());
        assert!(nest.cfg.lock().unwrap().spam.baseline_standing_publish);
    }

    #[tokio::test]
    async fn turning_standing_off_reads_back_no_baseline() {
        // Off withdraws (mail-spam.md § Cold start Path 2 → Standing publish):
        // after publish-now served a baseline, switching standing off re-reads
        // a state with nothing served.
        let m = MailPolicyMachine::new({
            let nest = FakeNest::new();
            *nest.baseline_agg.lock().unwrap() = (3, 42);
            Arc::new(nest)
        });
        m.hydrate().await.unwrap();
        m.dispatch(MailPolicyAction::SetBaselineStandingPublish { enabled: true })
            .await
            .unwrap();
        m.dispatch(MailPolicyAction::PublishSpamBaseline)
            .await
            .unwrap();
        assert!(m.snapshot().baseline_state.unwrap().published);
        m.dispatch(MailPolicyAction::SetBaselineStandingPublish { enabled: false })
            .await
            .unwrap();
        let state = m.snapshot().baseline_state.unwrap();
        assert!(!state.published);
        assert!(!state.standing);
        assert_eq!(state.published_at_ms, None);
    }

    #[tokio::test]
    async fn publish_now_keeps_its_result_and_refreshes_the_state() {
        // Publish-now's outcome stays on the page (no full refresh), and the
        // state text moves with it.
        let m = MailPolicyMachine::new({
            let nest = FakeNest::new();
            *nest.baseline_agg.lock().unwrap() = (3, 42);
            Arc::new(nest)
        });
        m.hydrate().await.unwrap();
        assert!(!m.snapshot().baseline_state.unwrap().published);
        m.dispatch(MailPolicyAction::PublishSpamBaseline)
            .await
            .unwrap();
        let snap = m.snapshot();
        assert!(snap.baseline_publish_result.unwrap().published);
        let state = snap.baseline_state.unwrap();
        assert!(state.published);
        assert_eq!(state.contributors, 3);
    }

    #[tokio::test]
    async fn a_deferred_publish_is_not_reported_as_withheld() {
        // The delta floor deferred the run: `published = false` must not read
        // as "too few contributors" — the view carries `deferred`, and the state
        // keeps serving what it had, marked deferred.
        let m = MailPolicyMachine::new({
            let nest = FakeNest::new();
            *nest.baseline_agg.lock().unwrap() = (3, 42);
            *nest.baseline_deferred.lock().unwrap() = true;
            Arc::new(nest)
        });
        m.hydrate().await.unwrap();
        m.dispatch(MailPolicyAction::PublishSpamBaseline)
            .await
            .unwrap();
        let snap = m.snapshot();
        let r = snap.baseline_publish_result.unwrap();
        assert!(!r.published);
        assert!(r.deferred);
        assert!(snap.baseline_state.unwrap().deferred);
    }
}
