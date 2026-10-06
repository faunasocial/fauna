//! Single-row admin overrides for the bridge `fetch_config` policy
//! sub-structs — the **write path** for the A3 Bucket-B catalog rollout
//! (tracked internally).
//!
//! There is one override table + one override struct per
//! `FetchConfigReply` sub-struct (`spam` / `auth` / `submission` /
//! `imap` / `outbound`), mirroring the five uniform
//! `fauna.bridges.put_<substruct>_policy` admin kinds. Each override
//! struct carries one `Option<T>` per sub-struct field:
//!
//! - `Some(v)` ⇒ set the value (overlaid onto the catalog default by
//!   `fetch_config_handler` before encoding).
//! - `None` ⇒ keep the wire-type catalog default
//!   (`SpamPolicyThresholds::default()` etc. in
//!   `libs/fauna-protocol/src/bridge_routing.rs`).
//!
//! Each override struct is persisted as a single JSON blob in its
//! single-row (`id = 1`) table, through the shared [`super::singleton`]
//! primitives — discrete columns buy nothing for a
//! single-row config table nobody queries by field, and the JSON shape
//! lets a later track grow a sub-struct's overrides without a migration
//! (lenient `#[serde(default)]` decode tolerates fields added since the
//! row was written). `Some(vec![])` for a list field is a meaningful
//! override (clear the list, e.g. air-gapped DNSBL) distinct from `None`.
//!
//! Was the 4-column `mail_policy` table (the DNS-perimeter slice, finding
//! #4); A3 Bucket B renamed it to
//! `mail_spam_policy`, grew it to the full sub-struct, and added the four
//! sibling tables. `mail.enabled` is **not** here — it is not a policy
//! override (`mail-policy-config.md` § Impl status; the s6 flag-file +
//! supervisor handshake is a separate lifecycle track).
//!
//! Spec: `docs/goal/behavior/mail-policy-config.md` § Policy catalog +
//! § Wire: `fauna.bridges.fetch_config` (subscribe-and-hot-reload still
//! deferred — a write takes effect on the bridge's next `fetch_config`).

use anyhow::Result;
use fauna_protocol::bridge_routing::{
    ImapPolicy, MassMailingPolicy, OutboundPolicy, SpamPolicyThresholds, SubmissionPolicyThresholds,
};
use serde::{Deserialize, Serialize};

use super::CacheDb;

/// Override for the `SpamPolicyThresholds` sub-struct
/// (`mail-policy-config.md` § Inbound perimeter). The bridge enforces
/// `max_score_before_spam_folder < max_score_before_reject`; the **handler**
/// validates the effective (override-or-default) ordering before writing —
/// the DB layer stores whatever it is given.
///
/// An unknown key is ignored on read and dropped on the next write.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SpamPolicyOverrides {
    pub max_score_before_spam_folder: Option<u32>,
    pub max_score_before_reject: Option<u32>,
    /// `Some(vec![])` clears DNSBL queries entirely (air-gapped deploy;
    /// CI fixtures); `None` keeps the `["zen.spamhaus.org"]` default.
    pub dnsbl_servers: Option<Vec<String>>,
    pub reject_no_rdns: Option<bool>,
    pub greylist_enabled: Option<bool>,
    pub greylist_delay_secs: Option<u32>,
    pub max_conn_per_min: Option<u32>,
    /// `Some("off")` / `Some("score_signal")` / `Some("enforce")`;
    /// string-typed for the same forward-compat reason as the wire field.
    pub fcrdns_mode: Option<String>,
    pub helo_identity_required: Option<bool>,
    pub reject_fcrdns_fail: Option<bool>,
    pub max_message_bytes: Option<u32>,
    /// Tier-2 per-user-Bayesian combined-score weight, milli (default 700).
    pub bayesian_weight_milli: Option<u32>,
    /// Cold-start sample floor below which the per-user term is 0 (default 50).
    pub bayesian_min_samples: Option<u32>,
    /// Confidence-ramp full-confidence + baseline-fade horizon (default 200).
    pub bayesian_full_confidence_samples: Option<u32>,
    /// `spam_training_history` retention in days for nest's daily GC (30).
    pub training_history_retention_days: Option<u32>,
    /// Recipient-whitelist penalty in points added to a catch-all recipient's
    /// combined score at the Go MTA loop (default 0 = off).
    pub unlisted_recipient_penalty: Option<u32>,
    /// Standing publish of the deployment spam baseline (default off). The
    /// put handler keeps the stored value when a request omits it — see
    /// `PutSpamPolicyRequest::baseline_standing_publish`.
    pub baseline_standing_publish: Option<bool>,
}

impl SpamPolicyOverrides {
    /// Resolve to the effective [`SpamPolicyThresholds`] nest-side reads
    /// consume, overlaying each `Some` override onto the wire-catalog
    /// `SpamPolicyThresholds::default()` — the same `None ⇒ catalog default`
    /// semantics as the Bucket-B `fetch_config` projection
    /// (`overlay_policy!` in `bridge_routing_handlers.rs`), applied nest-side
    /// where the deployment override would otherwise stay write-only. Mirrors
    /// [`ImapPolicyOverrides::effective`]; the per-user-Bayesian
    /// `full_confidence_samples` (the cold-start baseline fade horizon in
    /// `fetch_spam_model`'s `cold_start_seed`) and `training_history_retention_days`
    /// (the daily history GC) are the nest-side reads — the rest is resolved
    /// for completeness even though the MDA reads it Go-side via the already-
    /// overlaid `fetch_config`.
    pub fn effective(&self) -> SpamPolicyThresholds {
        let d = SpamPolicyThresholds::default();
        SpamPolicyThresholds {
            max_score_before_spam_folder: self
                .max_score_before_spam_folder
                .unwrap_or(d.max_score_before_spam_folder),
            max_score_before_reject: self
                .max_score_before_reject
                .unwrap_or(d.max_score_before_reject),
            dnsbl_servers: self.dnsbl_servers.clone().unwrap_or(d.dnsbl_servers),
            reject_no_rdns: self.reject_no_rdns.unwrap_or(d.reject_no_rdns),
            greylist_enabled: self.greylist_enabled.unwrap_or(d.greylist_enabled),
            greylist_delay_secs: self.greylist_delay_secs.unwrap_or(d.greylist_delay_secs),
            max_conn_per_min: self.max_conn_per_min.unwrap_or(d.max_conn_per_min),
            fcrdns_mode: self.fcrdns_mode.clone().unwrap_or(d.fcrdns_mode),
            helo_identity_required: self
                .helo_identity_required
                .unwrap_or(d.helo_identity_required),
            reject_fcrdns_fail: self.reject_fcrdns_fail.unwrap_or(d.reject_fcrdns_fail),
            max_message_bytes: self.max_message_bytes.unwrap_or(d.max_message_bytes),
            bayesian_weight_milli: self
                .bayesian_weight_milli
                .unwrap_or(d.bayesian_weight_milli),
            bayesian_min_samples: self.bayesian_min_samples.unwrap_or(d.bayesian_min_samples),
            bayesian_full_confidence_samples: self
                .bayesian_full_confidence_samples
                .unwrap_or(d.bayesian_full_confidence_samples),
            training_history_retention_days: self
                .training_history_retention_days
                .unwrap_or(d.training_history_retention_days),
            unlisted_recipient_penalty: self
                .unlisted_recipient_penalty
                .unwrap_or(d.unlisted_recipient_penalty),
            baseline_standing_publish: self
                .baseline_standing_publish
                .unwrap_or(d.baseline_standing_publish),
            // Forward-compat catch-all (transport.md rule 4): the effective
            // value is nest-minted from the catalog default + stored
            // overrides, so there is no peer `extra` to carry forward.
            extra: Default::default(),
        }
    }
}

/// Override for the `AuthPolicy` sub-struct (`mail-policy-config.md`
/// § Submission policy / DMARC enforcement gates).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthPolicyOverrides {
    pub enforce_dmarc: Option<bool>,
    pub enforce_dmarc_quarantine: Option<bool>,
    pub enforce_spf_hardfail: Option<bool>,
    pub enforce_dkim: Option<bool>,
    pub log_only: Option<bool>,
    pub max_auth_failures_per_minute: Option<u32>,
    pub max_conn_per_ip: Option<u32>,
}

/// Override for the `SubmissionPolicyThresholds` sub-struct.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SubmissionPolicyOverrides {
    pub max_per_day: Option<u32>,
    pub max_recipients_per_message: Option<u32>,
}

impl SubmissionPolicyOverrides {
    /// Resolve to the effective [`SubmissionPolicyThresholds`] nest-side
    /// enforcement reads consume, overlaying each `Some` override onto the
    /// wire-catalog `SubmissionPolicyThresholds::default()` — the same
    /// `None ⇒ catalog default` semantics as the Bucket-B `fetch_config`
    /// projection (`overlay_policy!` in `bridge_routing_handlers.rs`),
    /// applied nest-side where the deployment override would otherwise stay
    /// write-only. Mirrors [`SpamPolicyOverrides::effective`] /
    /// [`ImapPolicyOverrides::effective`]; both `max_per_day` and
    /// `max_recipients_per_message` are read nest-side by
    /// `check_submission_quota_handler` — **not** by the Go MTA. The
    /// per-message cap's *fast-path* check (`submission.go::Rcpt`) reads
    /// the submission token's `MaxRecipients`, which the client mints from
    /// its own hard-coded constant and never from this admin knob (sweep
    /// 169, `mail-policy-config.md` § Implementation status today); nest's
    /// `check_submission_quota` reply is the authoritative path.
    pub fn effective(&self) -> SubmissionPolicyThresholds {
        let d = SubmissionPolicyThresholds::default();
        SubmissionPolicyThresholds {
            max_per_day: self.max_per_day.unwrap_or(d.max_per_day),
            max_recipients_per_message: self
                .max_recipients_per_message
                .unwrap_or(d.max_recipients_per_message),
            // Forward-compat catch-all (transport.md rule 4): the effective
            // value is nest-minted from the catalog default + stored
            // overrides, so there is no peer `extra` to carry forward.
            extra: Default::default(),
        }
    }
}

/// Override for the `ImapPolicy` sub-struct.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImapPolicyOverrides {
    pub idle_timeout_secs: Option<u32>,
    pub tombstone_retention_days: Option<u32>,
    /// `"forbidden"` / `"allowed"`.
    pub delete_nonempty: Option<String>,
    pub bodystructure_cache_max: Option<u32>,
    pub storage_bytes_default: Option<u64>,
    pub message_count_default: Option<u32>,
}

impl ImapPolicyOverrides {
    /// Resolve to the effective [`ImapPolicy`] the nest-side quota /
    /// retention / delete-policy reads consume, overlaying each `Some`
    /// override onto the wire-catalog `ImapPolicy::default()`. Same
    /// `None ⇒ catalog default` semantics as the Bucket-B `fetch_config`
    /// projection (`overlay_policy!` in `bridge_routing_handlers.rs`) — but
    /// applied **nest-side**, where the deployment override would otherwise
    /// stay write-only (quota enforcement/reporting, `delete_nonempty`, and
    /// the CalDAV tombstone-retention read all consume the result). Mirrors
    /// [`AliasPolicyOverrides::effective`]; the `idle_timeout_secs` /
    /// `bodystructure_cache_max` knobs are also resolved here for
    /// completeness even though the MDA reads those Go-side via the
    /// already-overlaid `fetch_config`.
    pub fn effective(&self) -> ImapPolicy {
        let d = ImapPolicy::default();
        ImapPolicy {
            idle_timeout_secs: self.idle_timeout_secs.unwrap_or(d.idle_timeout_secs),
            tombstone_retention_days: self
                .tombstone_retention_days
                .unwrap_or(d.tombstone_retention_days),
            delete_nonempty: self.delete_nonempty.clone().unwrap_or(d.delete_nonempty),
            bodystructure_cache_max: self
                .bodystructure_cache_max
                .unwrap_or(d.bodystructure_cache_max),
            storage_bytes_default: self
                .storage_bytes_default
                .unwrap_or(d.storage_bytes_default),
            message_count_default: self
                .message_count_default
                .unwrap_or(d.message_count_default),
            // Forward-compat catch-all (transport.md rule 4): the effective
            // value is nest-minted from the catalog default + stored
            // overrides, so there is no peer `extra` to carry forward.
            extra: Default::default(),
        }
    }
}

/// Override for the `OutboundPolicy` sub-struct. List fields full-replace
/// when present.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OutboundPolicyOverrides {
    pub retry_schedule_seconds: Option<Vec<u64>>,
    pub permanent_failure_timeout_hours: Option<u32>,
    pub delay_warning_at_hours: Option<u32>,
    pub ndr_rate_limit_days: Option<u32>,
    pub suppress_ndr_spf_hardfail: Option<bool>,
    pub suppress_ndr_dmarc_reject: Option<bool>,
    pub postmaster_cc_bounces: Option<bool>,
    pub tlsrpt_send_reports: Option<bool>,
    pub ipv6_enabled: Option<bool>,
    pub treat_5xx_as_transient: Option<Vec<String>>,
}

impl OutboundPolicyOverrides {
    /// Resolve to the effective [`OutboundPolicy`] the nest-side retry
    /// scheduler (`outbound_retry::retry_policy_from_outbound`) consumes,
    /// overlaying each `Some` override onto the wire-catalog
    /// `OutboundPolicy::default()`. Same `None ⇒ catalog default` semantics
    /// as [`SubmissionPolicyOverrides::effective`] / [`ImapPolicyOverrides::
    /// effective`], applied nest-side where the deployment override would
    /// otherwise stay write-only (`mail-policy-config.md` § Implementation
    /// status today, sweep-169 finding: the retry scheduler's only
    /// production call site passed the compile-time default, never this
    /// override).
    ///
    /// `permanent_failure_timeout_hours` treats a stored literal `0` the
    /// same as `None` (mail-policy-config.md ruling 2): it is a **timing
    /// window**, not an allowance — `fauna_mail::outbound::retry` gives up
    /// once `wall_clock_elapsed >= permanent_failure_after`, so a `0` there
    /// would permanently fail every outbound message on its first attempt,
    /// never a coherent admin intent. Every other field keeps the plain
    /// `None ⇒ default` reading, including `retry_schedule_seconds:
    /// Some(vec![])`, which is the deliberate "no retries after attempt 1"
    /// clear (the `dnsbl_servers: Some(vec![])` precedent).
    pub fn effective(&self) -> OutboundPolicy {
        let d = OutboundPolicy::default();
        OutboundPolicy {
            retry_schedule_seconds: self
                .retry_schedule_seconds
                .clone()
                .unwrap_or(d.retry_schedule_seconds),
            permanent_failure_timeout_hours: self
                .permanent_failure_timeout_hours
                .filter(|&h| h != 0)
                .unwrap_or(d.permanent_failure_timeout_hours),
            delay_warning_at_hours: self
                .delay_warning_at_hours
                .unwrap_or(d.delay_warning_at_hours),
            ndr_rate_limit_days: self.ndr_rate_limit_days.unwrap_or(d.ndr_rate_limit_days),
            suppress_ndr_spf_hardfail: self
                .suppress_ndr_spf_hardfail
                .unwrap_or(d.suppress_ndr_spf_hardfail),
            suppress_ndr_dmarc_reject: self
                .suppress_ndr_dmarc_reject
                .unwrap_or(d.suppress_ndr_dmarc_reject),
            postmaster_cc_bounces: self
                .postmaster_cc_bounces
                .unwrap_or(d.postmaster_cc_bounces),
            tlsrpt_send_reports: self.tlsrpt_send_reports.unwrap_or(d.tlsrpt_send_reports),
            ipv6_enabled: self.ipv6_enabled.unwrap_or(d.ipv6_enabled),
            treat_5xx_as_transient: self
                .treat_5xx_as_transient
                .clone()
                .unwrap_or(d.treat_5xx_as_transient),
            // Forward-compat catch-all (transport.md rule 4): the effective
            // value is nest-minted from the catalog default + stored
            // overrides, so there is no peer `extra` to carry forward.
            extra: Default::default(),
        }
    }
}

/// Override for the four nest-side **alias-policy** knobs
/// (`mail-policy-config.md` § Inbound perimeter). Unlike the five
/// `fetch_config` sub-struct overrides above, these are **not** projected
/// to the bridge — the alias resolver (`resolve_recipient`) and alias CRUD
/// (`create_account_alias`) read them nest-side. [`Self::effective`]
/// resolves each `None` to the documented `fauna_mail::aliases` const
/// default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AliasPolicyOverrides {
    pub exact_aliases_max: Option<u32>,
    /// `Some(vec![])` clears the reservation entirely; `None` keeps the
    /// seven-name role-address default.
    pub reserved_local_parts: Option<Vec<String>>,
    pub subaddressing_enabled: Option<bool>,
    pub wildcard_prefix_enabled: Option<bool>,
}

/// The effective (override-or-default) alias policy the resolver / CRUD
/// handlers consume, produced by [`AliasPolicyOverrides::effective`].
/// Non-optional: every field has resolved to a concrete value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAliasPolicy {
    pub exact_aliases_max: u32,
    pub reserved_local_parts: Vec<String>,
    pub subaddressing_enabled: bool,
    pub wildcard_prefix_enabled: bool,
}

/// Override for the four mass-mailing ceilings (`mail-mass-mailing.md`
/// § Per-list rate accounting; the `mail.outbound.list_*` Tier-2 knobs).
/// Like the alias overrides, these are **not** enforced by the bridge — the
/// nest owns the `send_list_message` fan-out + per-list rate accounting and
/// reads [`Self::effective`] there (`MassMailingPolicy` is still projected to
/// the bridge in `fetch_config` for completeness, but at the catalog default).
/// The admin write RPC + UI land with the flat `admin-mail` page; until then
/// every field stays `None` ⇒ the [`MassMailingPolicy::default`] catalog value.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MassMailingPolicyOverrides {
    pub list_recipients_per_send_ceiling: Option<u64>,
    pub list_recipients_per_account_per_day_ceiling: Option<u64>,
    pub list_recipients_per_deployment_per_day_ceiling: Option<u64>,
    pub list_max_import_per_batch: Option<u32>,
}

impl MassMailingPolicyOverrides {
    /// Resolve each unset field to its [`MassMailingPolicy::default`] catalog
    /// value (same `None ⇒ catalog default` semantics as [`ImapPolicyOverrides::
    /// effective`], applied nest-side since these knobs gate the nest-owned
    /// list-send fan-out, not the bridge).
    pub fn effective(&self) -> MassMailingPolicy {
        let d = MassMailingPolicy::default();
        MassMailingPolicy {
            list_recipients_per_send_ceiling: self
                .list_recipients_per_send_ceiling
                .unwrap_or(d.list_recipients_per_send_ceiling),
            list_recipients_per_account_per_day_ceiling: self
                .list_recipients_per_account_per_day_ceiling
                .unwrap_or(d.list_recipients_per_account_per_day_ceiling),
            list_recipients_per_deployment_per_day_ceiling: self
                .list_recipients_per_deployment_per_day_ceiling
                .unwrap_or(d.list_recipients_per_deployment_per_day_ceiling),
            list_max_import_per_batch: self
                .list_max_import_per_batch
                .unwrap_or(d.list_max_import_per_batch),
            // Forward-compat catch-all (transport.md rule 4): the effective
            // value is nest-minted from the catalog default + stored
            // overrides, so there is no peer `extra` to carry forward.
            extra: Default::default(),
        }
    }
}

impl AliasPolicyOverrides {
    /// Resolve each unset field to its `fauna_mail::aliases` const default
    /// (the documented catalog default; same `None ⇒ default` semantics as
    /// the Bucket-B `fetch_config` overlay, but applied nest-side since
    /// these knobs are not projected to the bridge).
    pub fn effective(&self) -> ResolvedAliasPolicy {
        use fauna_mail::aliases::{
            DEFAULT_RESERVED_LOCAL_PARTS, EXACT_ALIASES_MAX_DEFAULT, SUBADDRESSING_ENABLED_DEFAULT,
            WILDCARD_PREFIX_ENABLED_DEFAULT,
        };
        ResolvedAliasPolicy {
            exact_aliases_max: self.exact_aliases_max.unwrap_or(EXACT_ALIASES_MAX_DEFAULT),
            reserved_local_parts: self.reserved_local_parts.clone().unwrap_or_else(|| {
                DEFAULT_RESERVED_LOCAL_PARTS
                    .iter()
                    .map(|s| (*s).to_string())
                    .collect()
            }),
            subaddressing_enabled: self
                .subaddressing_enabled
                .unwrap_or(SUBADDRESSING_ENABLED_DEFAULT),
            wildcard_prefix_enabled: self
                .wildcard_prefix_enabled
                .unwrap_or(WILDCARD_PREFIX_ENABLED_DEFAULT),
        }
    }
}

/// § Quota composition's per-session disk ceiling: "Default 10 GiB per export
/// session, admin-tunable (`mail.export.max_blob_bytes`)".
pub const EXPORT_MAX_BLOB_BYTES_DEFAULT: u64 = 10 * 1024 * 1024 * 1024;

/// § Quota composition's concurrency cap: "Default 3 concurrent in-flight
/// exports per user (`mail.export.max_concurrent_per_user`)".
pub const EXPORT_MAX_CONCURRENT_PER_USER_DEFAULT: u32 = 3;

/// Admin overrides for the two mailbox-export ceilings (the per-user footprint
/// is derived from them — [`ExportCeilings`])
/// (`mail-export.md` § Quota composition, catalogued in
/// `mail-policy-config.md` § Export (admin ceilings)).
///
/// These are the only two export values any human chooses, and both are
/// **admin** choices — so they rest in nest state, reachable from the admin
/// app, never in a config file, env var or CLI flag (`principles.md` § One
/// configuration surface). The admin write RPC + the `admin-mail` element are
/// gated on ui.yaml ratification and land together; until then every
/// deployment runs the catalog defaults, exactly as
/// [`MassMailingPolicyOverrides`] has since it landed. What this struct buys
/// now is that the *handlers already read the row* — so wiring the UI is a
/// write path plus an element, not a re-plumb of the export pipeline.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExportPolicyOverrides {
    pub max_blob_bytes: Option<u64>,
    pub max_concurrent_per_user: Option<u32>,
}

impl ExportPolicyOverrides {
    /// The effective ceilings: the override where one is set, the § Quota
    /// composition default otherwise — and the per-user footprint derived
    /// from the two.
    pub fn effective(&self) -> ExportCeilings {
        ExportCeilings::new(
            self.max_blob_bytes.unwrap_or(EXPORT_MAX_BLOB_BYTES_DEFAULT),
            self.max_concurrent_per_user
                .unwrap_or(EXPORT_MAX_CONCURRENT_PER_USER_DEFAULT),
        )
    }
}

/// § Quota composition's three bounds, in the one shape the export store
/// enforces them from (`db::mail_export`).
///
/// Two are the admin's choices above. The third, `held_bytes`, is **derived,
/// never chosen**: it is the "3 × 10 GiB = 30 GiB per user" the doc states,
/// made true. The concurrency cap alone never bounded disk, because a
/// finished export frees its slot while its blob rests for the whole § Expiry
/// window — so twelve completed 10 GiB exports sat under a "30 GiB" ceiling.
/// Deriving it keeps the bound the admin already reasons about (raise either
/// ceiling and the footprint follows) without a third knob to rationalize in
/// `mail-policy-config.md`, and keeps it apart from the mailbox storage quota,
/// which § Don't do these forbids export disk to touch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportCeilings {
    /// Per-session blob ceiling (`mail.export.max_blob_bytes`).
    pub blob_bytes: u64,
    /// Sessions in `running`/`paused` per user
    /// (`mail.export.max_concurrent_per_user`).
    pub concurrent: u32,
    /// Blob bytes per user, summed over every live session still holding a
    /// file: `blob_bytes × concurrent`.
    pub held_bytes: u64,
}

impl ExportCeilings {
    pub fn new(blob_bytes: u64, concurrent: u32) -> Self {
        Self {
            blob_bytes,
            concurrent,
            held_bytes: blob_bytes.saturating_mul(u64::from(concurrent)),
        }
    }
}

impl Default for ExportCeilings {
    fn default() -> Self {
        ExportPolicyOverrides::default().effective()
    }
}

impl CacheDb {
    pub async fn get_export_policy(&self) -> Result<ExportPolicyOverrides> {
        self.read_singleton_json("mail_export_policy").await
    }

    pub async fn put_export_policy(&self, overrides: ExportPolicyOverrides) -> Result<()> {
        self.write_singleton_json("mail_export_policy", &overrides)
            .await
    }

    pub async fn get_spam_policy(&self) -> Result<SpamPolicyOverrides> {
        self.read_singleton_json("mail_spam_policy").await
    }

    pub async fn put_spam_policy(&self, overrides: SpamPolicyOverrides) -> Result<()> {
        self.write_singleton_json("mail_spam_policy", &overrides)
            .await
    }

    pub async fn get_auth_policy(&self) -> Result<AuthPolicyOverrides> {
        self.read_singleton_json("mail_auth_policy").await
    }

    pub async fn put_auth_policy(&self, overrides: AuthPolicyOverrides) -> Result<()> {
        self.write_singleton_json("mail_auth_policy", &overrides)
            .await
    }

    pub async fn get_submission_policy(&self) -> Result<SubmissionPolicyOverrides> {
        self.read_singleton_json("mail_submission_policy").await
    }

    pub async fn put_submission_policy(&self, overrides: SubmissionPolicyOverrides) -> Result<()> {
        self.write_singleton_json("mail_submission_policy", &overrides)
            .await
    }

    pub async fn get_imap_policy(&self) -> Result<ImapPolicyOverrides> {
        self.read_singleton_json("mail_imap_policy").await
    }

    pub async fn put_imap_policy(&self, overrides: ImapPolicyOverrides) -> Result<()> {
        self.write_singleton_json("mail_imap_policy", &overrides)
            .await
    }

    pub async fn get_outbound_policy(&self) -> Result<OutboundPolicyOverrides> {
        self.read_singleton_json("mail_outbound_policy").await
    }

    pub async fn put_outbound_policy(&self, overrides: OutboundPolicyOverrides) -> Result<()> {
        self.write_singleton_json("mail_outbound_policy", &overrides)
            .await
    }

    pub async fn get_alias_policy(&self) -> Result<AliasPolicyOverrides> {
        self.read_singleton_json("mail_alias_policy").await
    }

    pub async fn put_alias_policy(&self, overrides: AliasPolicyOverrides) -> Result<()> {
        self.write_singleton_json("mail_alias_policy", &overrides)
            .await
    }

    pub async fn get_mass_mailing_policy(&self) -> Result<MassMailingPolicyOverrides> {
        self.read_singleton_json("mail_mass_mailing_policy").await
    }

    pub async fn put_mass_mailing_policy(
        &self,
        overrides: MassMailingPolicyOverrides,
    ) -> Result<()> {
        self.write_singleton_json("mail_mass_mailing_policy", &overrides)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_get_returns_default_for_each_substruct() {
        let db = CacheDb::open_in_memory().unwrap();
        assert_eq!(
            db.get_spam_policy().await.unwrap(),
            SpamPolicyOverrides::default()
        );
        assert_eq!(
            db.get_auth_policy().await.unwrap(),
            AuthPolicyOverrides::default()
        );
        assert_eq!(
            db.get_submission_policy().await.unwrap(),
            SubmissionPolicyOverrides::default()
        );
        assert_eq!(
            db.get_imap_policy().await.unwrap(),
            ImapPolicyOverrides::default()
        );
        assert_eq!(
            db.get_outbound_policy().await.unwrap(),
            OutboundPolicyOverrides::default()
        );
    }

    #[tokio::test]
    async fn spam_put_then_get_round_trips_full() {
        let db = CacheDb::open_in_memory().unwrap();
        let put = SpamPolicyOverrides {
            max_score_before_spam_folder: Some(4),
            max_score_before_reject: Some(14),
            dnsbl_servers: Some(vec!["bl1.example".into()]),
            reject_no_rdns: Some(true),
            greylist_enabled: Some(false),
            greylist_delay_secs: Some(0),
            max_conn_per_min: Some(25),
            fcrdns_mode: Some("off".into()),
            helo_identity_required: Some(false),
            reject_fcrdns_fail: Some(true),
            max_message_bytes: Some(100_000_000),
            bayesian_weight_milli: Some(600),
            bayesian_min_samples: Some(40),
            bayesian_full_confidence_samples: Some(400),
            training_history_retention_days: Some(60),
            unlisted_recipient_penalty: Some(7),
            baseline_standing_publish: Some(true),
        };
        db.put_spam_policy(put.clone()).await.unwrap();
        assert_eq!(db.get_spam_policy().await.unwrap(), put);
        // The override flows through to the effective wire policy.
        assert_eq!(
            db.get_spam_policy()
                .await
                .unwrap()
                .effective()
                .unlisted_recipient_penalty,
            7
        );
    }

    #[tokio::test]
    async fn spam_put_then_get_round_trips_empty_dnsbl_distinct_from_none() {
        let db = CacheDb::open_in_memory().unwrap();
        let put = SpamPolicyOverrides {
            dnsbl_servers: Some(Vec::new()),
            ..Default::default()
        };
        db.put_spam_policy(put).await.unwrap();
        let got = db.get_spam_policy().await.unwrap();
        assert_eq!(got.dnsbl_servers, Some(Vec::<String>::new()));
        assert!(got.greylist_enabled.is_none());
    }

    #[tokio::test]
    async fn spam_put_is_idempotent_overwrite_on_fixed_row() {
        let db = CacheDb::open_in_memory().unwrap();
        db.put_spam_policy(SpamPolicyOverrides {
            max_score_before_reject: Some(99),
            ..Default::default()
        })
        .await
        .unwrap();
        db.put_spam_policy(SpamPolicyOverrides {
            max_score_before_reject: Some(20),
            greylist_enabled: Some(false),
            ..Default::default()
        })
        .await
        .unwrap();
        let got = db.get_spam_policy().await.unwrap();
        assert_eq!(got.max_score_before_reject, Some(20));
        assert_eq!(got.greylist_enabled, Some(false));
    }

    #[tokio::test]
    async fn auth_put_then_get_round_trips() {
        let db = CacheDb::open_in_memory().unwrap();
        let put = AuthPolicyOverrides {
            enforce_dmarc: Some(false),
            enforce_dkim: Some(true),
            log_only: Some(true),
            max_auth_failures_per_minute: Some(15),
            ..Default::default()
        };
        db.put_auth_policy(put.clone()).await.unwrap();
        assert_eq!(db.get_auth_policy().await.unwrap(), put);
    }

    #[tokio::test]
    async fn submission_put_then_get_round_trips() {
        let db = CacheDb::open_in_memory().unwrap();
        let put = SubmissionPolicyOverrides {
            max_per_day: Some(500),
            max_recipients_per_message: Some(50),
        };
        db.put_submission_policy(put.clone()).await.unwrap();
        assert_eq!(db.get_submission_policy().await.unwrap(), put);
    }

    #[tokio::test]
    async fn submission_effective_uses_catalog_default_when_unset() {
        // An empty override row resolves to the wire-catalog
        // `SubmissionPolicyThresholds`.
        let db = CacheDb::open_in_memory().unwrap();
        let eff = db.get_submission_policy().await.unwrap().effective();
        assert_eq!(eff, SubmissionPolicyThresholds::default());
    }

    #[tokio::test]
    async fn submission_effective_overlays_each_some_field() {
        // An admin-lowered `max_per_day` (and `max_recipients_per_message`)
        // actually binds — this is the read path that makes
        // `put_submission_policy` more than write-only
        // (`check_submission_quota_handler` consumes `.effective()`).
        let db = CacheDb::open_in_memory().unwrap();
        db.put_submission_policy(SubmissionPolicyOverrides {
            max_per_day: Some(3),
            ..Default::default()
        })
        .await
        .unwrap();
        let eff = db.get_submission_policy().await.unwrap().effective();
        assert_eq!(eff.max_per_day, 3, "override wins");
        assert_eq!(
            eff.max_recipients_per_message,
            SubmissionPolicyThresholds::default().max_recipients_per_message,
            "unset field keeps catalog default"
        );
    }

    #[tokio::test]
    async fn imap_put_then_get_round_trips_incl_u64_storage() {
        let db = CacheDb::open_in_memory().unwrap();
        let put = ImapPolicyOverrides {
            idle_timeout_secs: Some(900),
            tombstone_retention_days: Some(14),
            delete_nonempty: Some("allowed".into()),
            bodystructure_cache_max: Some(8192),
            storage_bytes_default: Some(2 << 30),
            message_count_default: Some(100_000),
        };
        db.put_imap_policy(put.clone()).await.unwrap();
        assert_eq!(db.get_imap_policy().await.unwrap(), put);
    }

    #[tokio::test]
    async fn imap_effective_uses_catalog_default_when_unset() {
        // An empty override row resolves to the wire-catalog `ImapPolicy`.
        let db = CacheDb::open_in_memory().unwrap();
        let eff = db.get_imap_policy().await.unwrap().effective();
        assert_eq!(eff, ImapPolicy::default());
    }

    #[tokio::test]
    async fn imap_effective_overlays_each_some_field() {
        // A lowered deployment quota (and the other knobs) actually bind:
        // every `Some` override wins, every `None` field keeps the catalog
        // default. This is the read path that makes `put_imap_policy` more
        // than write-only.
        let db = CacheDb::open_in_memory().unwrap();
        db.put_imap_policy(ImapPolicyOverrides {
            storage_bytes_default: Some(4096),
            delete_nonempty: Some("allowed".into()),
            tombstone_retention_days: Some(14),
            // message_count_default / idle_timeout_secs /
            // bodystructure_cache_max left unset → catalog default.
            ..Default::default()
        })
        .await
        .unwrap();
        let eff = db.get_imap_policy().await.unwrap().effective();
        assert_eq!(eff.storage_bytes_default, 4096, "override wins");
        assert_eq!(eff.delete_nonempty, "allowed", "override wins");
        assert_eq!(eff.tombstone_retention_days, 14, "override wins");
        assert_eq!(
            eff.message_count_default,
            ImapPolicy::default().message_count_default,
            "unset field keeps catalog default"
        );
        assert_eq!(
            eff.idle_timeout_secs,
            ImapPolicy::default().idle_timeout_secs,
            "unset field keeps catalog default"
        );
    }

    #[tokio::test]
    async fn outbound_put_then_get_round_trips_incl_lists() {
        let db = CacheDb::open_in_memory().unwrap();
        let put = OutboundPolicyOverrides {
            retry_schedule_seconds: Some(vec![0, 600, 3600]),
            permanent_failure_timeout_hours: Some(72),
            delay_warning_at_hours: Some(2),
            ndr_rate_limit_days: Some(3),
            suppress_ndr_spf_hardfail: Some(false),
            suppress_ndr_dmarc_reject: Some(false),
            postmaster_cc_bounces: Some(false),
            tlsrpt_send_reports: Some(false),
            ipv6_enabled: Some(false),
            treat_5xx_as_transient: Some(vec!["5.7.1".into()]),
        };
        db.put_outbound_policy(put.clone()).await.unwrap();
        assert_eq!(db.get_outbound_policy().await.unwrap(), put);
    }

    #[tokio::test]
    async fn outbound_effective_uses_catalog_default_when_unset() {
        // An empty override row resolves to the wire-catalog `OutboundPolicy`
        // — the retry scheduler's fallback when no admin override exists.
        let db = CacheDb::open_in_memory().unwrap();
        let eff = db.get_outbound_policy().await.unwrap().effective();
        assert_eq!(eff, OutboundPolicy::default());
    }

    #[tokio::test]
    async fn outbound_effective_overlays_each_some_field() {
        // An admin-set retry schedule actually binds — this is the read path
        // that makes `put_outbound_policy` more than write-only
        // (`outbound_retry::retry_policy_from_outbound` consumes `.effective()`
        // via `mark_outbound_failed_handler`, mail-policy-config.md sweep-169).
        let db = CacheDb::open_in_memory().unwrap();
        db.put_outbound_policy(OutboundPolicyOverrides {
            retry_schedule_seconds: Some(vec![0, 60]),
            ..Default::default()
        })
        .await
        .unwrap();
        let eff = db.get_outbound_policy().await.unwrap().effective();
        assert_eq!(eff.retry_schedule_seconds, vec![0, 60], "override wins");
        assert_eq!(
            eff.permanent_failure_timeout_hours,
            OutboundPolicy::default().permanent_failure_timeout_hours,
            "unset field keeps catalog default"
        );
    }

    #[tokio::test]
    async fn outbound_effective_treats_zero_permanent_failure_timeout_as_unset() {
        // `permanent_failure_timeout_hours = 0` is a timing window, not an
        // allowance (mail-policy-config.md ruling 2): a literal 0 would
        // permanently fail every outbound message on its first attempt —
        // never a coherent admin intent. The stored 0 must resolve to the
        // catalog default, not ride through as a live 0.
        let db = CacheDb::open_in_memory().unwrap();
        db.put_outbound_policy(OutboundPolicyOverrides {
            permanent_failure_timeout_hours: Some(0),
            ..Default::default()
        })
        .await
        .unwrap();
        let eff = db.get_outbound_policy().await.unwrap().effective();
        assert_eq!(
            eff.permanent_failure_timeout_hours,
            OutboundPolicy::default().permanent_failure_timeout_hours,
            "a stored 0 must fall back to the catalog default, not survive as a live 0"
        );
    }

    #[tokio::test]
    async fn alias_policy_empty_get_default_and_effective_uses_consts() {
        use fauna_mail::aliases::{
            DEFAULT_RESERVED_LOCAL_PARTS, EXACT_ALIASES_MAX_DEFAULT, SUBADDRESSING_ENABLED_DEFAULT,
            WILDCARD_PREFIX_ENABLED_DEFAULT,
        };
        let db = CacheDb::open_in_memory().unwrap();
        assert_eq!(
            db.get_alias_policy().await.unwrap(),
            AliasPolicyOverrides::default()
        );
        let eff = db.get_alias_policy().await.unwrap().effective();
        assert_eq!(eff.exact_aliases_max, EXACT_ALIASES_MAX_DEFAULT);
        assert_eq!(eff.subaddressing_enabled, SUBADDRESSING_ENABLED_DEFAULT);
        assert_eq!(eff.wildcard_prefix_enabled, WILDCARD_PREFIX_ENABLED_DEFAULT);
        assert_eq!(
            eff.reserved_local_parts.len(),
            DEFAULT_RESERVED_LOCAL_PARTS.len()
        );
    }

    #[tokio::test]
    async fn alias_policy_put_then_get_round_trips_and_effective_overrides() {
        let db = CacheDb::open_in_memory().unwrap();
        let put = AliasPolicyOverrides {
            exact_aliases_max: Some(5),
            reserved_local_parts: Some(vec!["postmaster".into(), "sales".into()]),
            subaddressing_enabled: Some(false),
            wildcard_prefix_enabled: Some(true),
        };
        db.put_alias_policy(put.clone()).await.unwrap();
        assert_eq!(db.get_alias_policy().await.unwrap(), put);
        let eff = db.get_alias_policy().await.unwrap().effective();
        assert_eq!(eff.exact_aliases_max, 5);
        assert_eq!(
            eff.reserved_local_parts,
            vec!["postmaster".to_string(), "sales".to_string()]
        );
        assert!(!eff.subaddressing_enabled);
        assert!(eff.wildcard_prefix_enabled);
    }

    #[tokio::test]
    async fn mass_mailing_empty_get_default_and_effective_uses_catalog() {
        let db = CacheDb::open_in_memory().unwrap();
        assert_eq!(
            db.get_mass_mailing_policy().await.unwrap(),
            MassMailingPolicyOverrides::default()
        );
        let eff = db.get_mass_mailing_policy().await.unwrap().effective();
        assert_eq!(eff, MassMailingPolicy::default());
    }

    #[tokio::test]
    async fn mass_mailing_put_then_get_round_trips_and_effective_overrides() {
        let db = CacheDb::open_in_memory().unwrap();
        let put = MassMailingPolicyOverrides {
            list_recipients_per_send_ceiling: Some(2_000),
            list_recipients_per_deployment_per_day_ceiling: Some(100_000),
            // per-account ceiling + import batch left unset → catalog default.
            ..Default::default()
        };
        db.put_mass_mailing_policy(put.clone()).await.unwrap();
        assert_eq!(db.get_mass_mailing_policy().await.unwrap(), put);
        let eff = db.get_mass_mailing_policy().await.unwrap().effective();
        assert_eq!(eff.list_recipients_per_send_ceiling, 2_000, "override wins");
        assert_eq!(
            eff.list_recipients_per_deployment_per_day_ceiling, 100_000,
            "override wins"
        );
        assert_eq!(
            eff.list_recipients_per_account_per_day_ceiling,
            MassMailingPolicy::default().list_recipients_per_account_per_day_ceiling,
            "unset field keeps catalog default"
        );
        assert_eq!(
            eff.list_max_import_per_batch,
            MassMailingPolicy::default().list_max_import_per_batch,
            "unset field keeps catalog default"
        );
    }

    #[tokio::test]
    async fn alias_policy_empty_reserved_clears_distinct_from_none() {
        use fauna_mail::aliases::EXACT_ALIASES_MAX_DEFAULT;
        let db = CacheDb::open_in_memory().unwrap();
        db.put_alias_policy(AliasPolicyOverrides {
            reserved_local_parts: Some(Vec::new()),
            ..Default::default()
        })
        .await
        .unwrap();
        let eff = db.get_alias_policy().await.unwrap().effective();
        // Some(vec![]) clears the reservation entirely…
        assert!(eff.reserved_local_parts.is_empty());
        // …while unset fields still fall back to the const default.
        assert_eq!(eff.exact_aliases_max, EXACT_ALIASES_MAX_DEFAULT);
    }
}
