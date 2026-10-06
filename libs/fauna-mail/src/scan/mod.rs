//! Perimeter content-scan logic: ClamAV verdict + rspamd score, as **pure**
//! functions over the daemons' wire replies.
//!
//! This is a **stage-1 perimeter scorer** per
//! `docs/goal/architecture/content-scoring.md`: it runs at the mail-bridge
//! (MTA) on the plaintext the bridge holds pre-seal, in both storage modes; the
//! nest never sees the plaintext and only stores the verdict metadata
//! (`message_scan_results`, rides `ingest_inbound_mail`). See
//! `docs/goal/behavior/mail-content-scanning.md`.
//!
//! Like the spam gate (`crate::spam`), the **network I/O is Go-side** in the
//! bridge — Go dials clamd (unix socket / TCP loopback) and POSTs to rspamd
//! `/checkv2`; this module only frames-free-parses the replies and decides the
//! delivery action. Keeping it pure (no tokio, no reqwest) is what lets it sit
//! in the FFI surface (`fauna-ffi` builds `--no-default-features`, which
//! excludes `fauna-mail`'s reqwest-bearing `outbound-net`).
//!
//! **No floats cross the dag-cbor wire** (`serialization.md` strict decode
//! rejects them): rspamd scores are carried as **milli-ints** (`score * 1000`,
//! rounded), mirroring the FilterRule per-mille precedent.

use serde::{Deserialize, Serialize};

// This module owns the *parsing* and the delivery decision; the scan RESULT
// types live once in `fauna_core::mail_scan`, which owns their wire-shape
// contract and the UniFFI derives the Go MTA's binding needs, and are
// re-exported here so every `fauna_mail::scan::ClamavVerdict` path keeps
// working.
//
// They used to be defined here *and* in `fauna_protocol::bridge_routing`,
// hand-mirrored — and, unlike the auth verdicts row 163 unified, the two copies
// did **not** agree: this one carried no serde container attributes at all and
// no `Default`, while protocol's carried the adjacent tagging and `Default` but
// none of the UniFFI derives. That divergence was latent rather than live (this
// copy's serde impl is dead on the wire — the Go MTA hand-builds the wire shape),
// so the unified type is a strict union and no byte moves. One definition makes
// the drift unrepresentable (pinned by
// `the_scan_types_have_exactly_one_definition` in `tests/scan_tests.rs`).
pub use fauna_core::mail_scan::{ClamavVerdict, RspamdRuleContribution, RspamdScore};

/// Admin action when ClamAV finds malware (`mail.scanning.clamav_action_on_infected`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ClamavAction {
    /// Default — `554 5.7.1` at the SMTP perimeter; message never stored.
    Reject,
    /// Deliver to the recipient's Junk (still records the row).
    Junk,
    /// Deliver with `X-Fauna-Scan-Clamav: infected` headers; no routing override.
    Tag,
}

/// Scan policy projected from nest config (the bridge passes it in).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ScanPolicy {
    pub clamav_enabled: bool,
    pub clamav_action_on_infected: ClamavAction,
    pub rspamd_enabled: bool,
    /// rspamd raw→scaled multiplier × 1000 (default 500 = 0.5 — maps rspamd's
    /// nominal 30 to our 15).
    pub rspamd_score_scaling_per_mille: u16,
}

impl Default for ScanPolicy {
    fn default() -> Self {
        Self {
            clamav_enabled: true,
            clamav_action_on_infected: ClamavAction::Reject,
            rspamd_enabled: true,
            rspamd_score_scaling_per_mille: 500,
        }
    }
}

/// Default retention for `message_scan_results` rows (`mail-content-scanning.md`
/// § Retention — "Default 30 days. Admin-tunable via
/// `mail.scanning.result_retention_days` (Tier 2)"). Hardcoded here (the
/// `mail.scanning.*` Tier-2 knobs are catalogued but inert — neither projected
/// to the bridge nor admin-writable yet, like [`ScanPolicy`]'s own defaults)
/// until the scanning-policy write-path lands the admin knob; the nest spawns a
/// periodic sweeper deleting rows older than this. The
/// `mail.scanning.rejected_malware_retention_days` per-action-category override
/// (§ Retention with action_taken = 'rejected_malware') defaults to null =
/// falls back to this value.
pub const SCAN_RESULT_RETENTION_DAYS: i64 = 30;

/// The delivery action the bridge must take, after the ClamAV verdict.
///
/// rspamd's score does **not** gate delivery in T1.4 (it is stored + header-
/// stamped only; the `max(rspamd, weighted_bayesian)` feed into disposition is
/// T3.1, `mail-spam.md` § Combined-score formula). Only ClamAV gates here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ScanAction {
    /// Stamp scan headers, seal, deliver.
    Deliver,
    /// `554 5.7.1 Message contains malware: <signature>` at the perimeter.
    RejectMalware { signature: String },
    /// Deliver to the recipient's Junk, with infected headers.
    Junk { signature: String },
    /// Deliver with infected headers, no routing override.
    Tag { signature: String },
    /// `451 4.7.0` tempfail — scanner errored. Never allow-without-scan.
    Tempfail { reason: String },
}

/// Error parsing an rspamd `/checkv2` response. The bridge maps this to a
/// `451` tempfail (never allow-without-score).
#[derive(Debug, thiserror::Error)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Error))]
#[cfg_attr(feature = "uniffi", uniffi(flat_error))]
pub enum ScanError {
    #[error("rspamd response was not valid JSON: {0}")]
    InvalidJson(String),
    #[error("rspamd response missing required field: {0}")]
    MissingField(String),
}

/// Parse a clamd `zINSTREAM` reply into a verdict.
///
/// clamd replies (null-terminated): `stream: OK`, `stream: <sig> FOUND`, or an
/// `... ERROR` line. Anything unrecognized is treated as an `Error` (never
/// silently `Clean` — that was the legacy `clamd.rs` fail-open bug).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn clamd_parse_reply(reply: &str) -> ClamavVerdict {
    let trimmed = reply.trim_matches(char::from(0)).trim();

    if let Some(stripped) = trimmed.strip_suffix(" FOUND") {
        let signature = stripped
            .strip_prefix("stream: ")
            .unwrap_or(stripped)
            .trim()
            .to_string();
        return ClamavVerdict::Infected { signature };
    }

    if trimmed.contains("ERROR") {
        return ClamavVerdict::Error {
            detail: trimmed.to_string(),
        };
    }

    // clamd's clean reply is exactly "stream: OK".
    if trimmed.ends_with("OK") {
        return ClamavVerdict::Clean;
    }

    ClamavVerdict::Error {
        detail: format!("unrecognized clamd reply: {trimmed}"),
    }
}

/// Parse an rspamd `/checkv2` JSON response, applying `scaling_per_mille`.
///
/// All scores are returned as milli-ints. `symbols` is optional (a clean
/// message with no fired rules has none); a missing top-level `score` is an
/// error (a malformed 200 → tempfail, never allow-without-score).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn rspamd_parse_reply(json: &str, scaling_per_mille: u16) -> Result<RspamdScore, ScanError> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| ScanError::InvalidJson(e.to_string()))?;

    let raw_score = v
        .get("score")
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| ScanError::MissingField("score".to_string()))?;

    let raw_milli = (raw_score * 1000.0).round() as i32;
    // scaled = raw * (scaling_per_mille / 1000); ×1000 → raw * scaling_per_mille.
    let scaled_milli = (raw_score * f64::from(scaling_per_mille)).round() as i32;

    let mut breakdown: Vec<RspamdRuleContribution> = Vec::new();
    if let Some(symbols) = v.get("symbols").and_then(serde_json::Value::as_object) {
        for (rule, sym) in symbols {
            let score_milli = sym
                .get("score")
                .and_then(serde_json::Value::as_f64)
                .map(|s| (s * 1000.0).round() as i32)
                .unwrap_or(0);
            breakdown.push(RspamdRuleContribution {
                rule: rule.clone(),
                score_milli,
            });
        }
    }
    // Deterministic order (serde_json object iteration is insertion/arbitrary).
    breakdown.sort_by(|a, b| a.rule.cmp(&b.rule));
    let flagged_rules = breakdown.iter().map(|c| c.rule.clone()).collect();

    Ok(RspamdScore {
        raw_milli,
        scaled_milli,
        flagged_rules,
        breakdown,
    })
}

/// Decide the delivery action from the ClamAV verdict + policy.
///
/// Centralizes the never-allow-without-scan rule: an `Error` verdict becomes a
/// `Tempfail` (the bridge 451s and the sender's MTA retries). `Clean` and
/// `BypassedOversize` deliver. Only ClamAV gates delivery in T1.4; rspamd's
/// score is stored/header-stamped separately.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn decide_scan_action(clamav: &ClamavVerdict, policy: &ScanPolicy) -> ScanAction {
    match clamav {
        // A message the scanner never saw is not a scanner outcome to gate on:
        // the never-allow-without-scan rule governs a scanner that RAN.
        ClamavVerdict::Clean | ClamavVerdict::BypassedOversize | ClamavVerdict::NotScanned => {
            ScanAction::Deliver
        }
        ClamavVerdict::Error { detail } => ScanAction::Tempfail {
            reason: detail.clone(),
        },
        ClamavVerdict::Infected { signature } => match policy.clamav_action_on_infected {
            ClamavAction::Reject => ScanAction::RejectMalware {
                signature: signature.clone(),
            },
            ClamavAction::Junk => ScanAction::Junk {
                signature: signature.clone(),
            },
            ClamavAction::Tag => ScanAction::Tag {
                signature: signature.clone(),
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamd_clean_reply() {
        assert_eq!(clamd_parse_reply("stream: OK"), ClamavVerdict::Clean);
    }

    #[test]
    fn clamd_clean_reply_strips_null_terminator() {
        assert_eq!(clamd_parse_reply("stream: OK\0"), ClamavVerdict::Clean);
    }

    #[test]
    fn clamd_infected_reply_strips_stream_prefix() {
        assert_eq!(
            clamd_parse_reply("stream: Win.Test.EICAR_HDB-1 FOUND"),
            ClamavVerdict::Infected {
                signature: "Win.Test.EICAR_HDB-1".to_string()
            }
        );
    }

    #[test]
    fn clamd_infected_reply_without_stream_prefix_keeps_full_text_as_signature() {
        assert_eq!(
            clamd_parse_reply("Eicar-Test-Signature FOUND"),
            ClamavVerdict::Infected {
                signature: "Eicar-Test-Signature".to_string()
            }
        );
    }

    #[test]
    fn clamd_error_reply() {
        assert_eq!(
            clamd_parse_reply("stream: UNKNOWN COMMAND ERROR"),
            ClamavVerdict::Error {
                detail: "stream: UNKNOWN COMMAND ERROR".to_string()
            }
        );
    }

    #[test]
    fn clamd_empty_reply_is_an_error_never_clean() {
        assert_eq!(
            clamd_parse_reply(""),
            ClamavVerdict::Error {
                detail: "unrecognized clamd reply: ".to_string()
            }
        );
    }

    #[test]
    fn clamd_garbage_reply_is_an_error_never_clean() {
        assert_eq!(
            clamd_parse_reply("garbage"),
            ClamavVerdict::Error {
                detail: "unrecognized clamd reply: garbage".to_string()
            }
        );
    }

    /// Pins the current loose `ends_with("OK")` match: anything merely ending in
    /// "OK" reads as clean, not only the exact "stream: OK" reply. Documents a
    /// real looseness rather than asserting it is desirable.
    #[test]
    fn clamd_reply_merely_ending_in_ok_is_read_as_clean() {
        assert_eq!(clamd_parse_reply("randomtextOK"), ClamavVerdict::Clean);
    }

    /// The " FOUND"-suffix check runs before the "ERROR"-substring check, so a
    /// signature that itself contains the word ERROR still reads as Infected.
    #[test]
    fn clamd_found_suffix_takes_precedence_over_error_substring() {
        assert_eq!(
            clamd_parse_reply("stream: ERROR FOUND"),
            ClamavVerdict::Infected {
                signature: "ERROR".to_string()
            }
        );
    }

    #[test]
    fn rspamd_reply_without_symbols_has_empty_breakdown() {
        let score = rspamd_parse_reply(r#"{"score": 5.0}"#, 500).expect("valid reply");
        assert_eq!(
            score,
            RspamdScore {
                raw_milli: 5000,
                scaled_milli: 2500,
                flagged_rules: vec![],
                breakdown: vec![],
            }
        );
    }

    #[test]
    fn rspamd_malformed_json_is_invalid_json_error() {
        assert!(matches!(
            rspamd_parse_reply("not json", 500),
            Err(ScanError::InvalidJson(_))
        ));
    }

    #[test]
    fn rspamd_missing_score_field_is_a_missing_field_error() {
        assert!(matches!(
            rspamd_parse_reply(r#"{"symbols": {}}"#, 500),
            Err(ScanError::MissingField(field)) if field == "score"
        ));
    }

    #[test]
    fn rspamd_non_numeric_score_is_a_missing_field_error() {
        // `as_f64()` returns `None` for a JSON string — same fail-closed path as
        // an absent field, never a silent 0.
        assert!(matches!(
            rspamd_parse_reply(r#"{"score": "5.0"}"#, 500),
            Err(ScanError::MissingField(field)) if field == "score"
        ));
    }

    #[test]
    fn rspamd_negative_score_rounds_correctly() {
        let score = rspamd_parse_reply(r#"{"score": -2.3}"#, 500).expect("valid reply");
        assert_eq!(score.raw_milli, -2300);
        assert_eq!(score.scaled_milli, -1150);
    }

    #[test]
    fn rspamd_symbols_breakdown_sorted_with_missing_score_defaulting_to_zero() {
        let json = r#"{
            "score": 5.0,
            "symbols": {
                "HTML_ONLY": {},
                "BAYES_SPAM": {"score": 3.5}
            }
        }"#;
        let score = rspamd_parse_reply(json, 500).expect("valid reply");
        assert_eq!(score.flagged_rules, vec!["BAYES_SPAM", "HTML_ONLY"]);
        assert_eq!(
            score.breakdown,
            vec![
                RspamdRuleContribution {
                    rule: "BAYES_SPAM".to_string(),
                    score_milli: 3500,
                },
                RspamdRuleContribution {
                    rule: "HTML_ONLY".to_string(),
                    score_milli: 0,
                },
            ]
        );
    }

    #[test]
    fn rspamd_symbols_as_a_json_array_is_ignored_not_an_error() {
        let score = rspamd_parse_reply(r#"{"score": 5.0, "symbols": []}"#, 500)
            .expect("a malformed `symbols` shape must not fail the whole parse");
        assert_eq!(score.breakdown, vec![]);
        assert_eq!(score.flagged_rules, Vec::<String>::new());
    }

    #[test]
    fn decide_scan_action_delivers_clean_bypassed_and_not_scanned() {
        let policy = ScanPolicy::default();
        assert_eq!(
            decide_scan_action(&ClamavVerdict::Clean, &policy),
            ScanAction::Deliver
        );
        assert_eq!(
            decide_scan_action(&ClamavVerdict::BypassedOversize, &policy),
            ScanAction::Deliver
        );
        assert_eq!(
            decide_scan_action(&ClamavVerdict::NotScanned, &policy),
            ScanAction::Deliver
        );
    }

    #[test]
    fn decide_scan_action_never_allows_without_scan_on_error() {
        let policy = ScanPolicy::default();
        assert_eq!(
            decide_scan_action(
                &ClamavVerdict::Error {
                    detail: "clamd unreachable".to_string()
                },
                &policy
            ),
            ScanAction::Tempfail {
                reason: "clamd unreachable".to_string()
            }
        );
    }

    #[test]
    fn decide_scan_action_infected_follows_the_configured_policy() {
        let infected = ClamavVerdict::Infected {
            signature: "Win.Test.EICAR".to_string(),
        };
        let reject = ScanPolicy {
            clamav_action_on_infected: ClamavAction::Reject,
            ..ScanPolicy::default()
        };
        let junk = ScanPolicy {
            clamav_action_on_infected: ClamavAction::Junk,
            ..ScanPolicy::default()
        };
        let tag = ScanPolicy {
            clamav_action_on_infected: ClamavAction::Tag,
            ..ScanPolicy::default()
        };
        assert_eq!(
            decide_scan_action(&infected, &reject),
            ScanAction::RejectMalware {
                signature: "Win.Test.EICAR".to_string()
            }
        );
        assert_eq!(
            decide_scan_action(&infected, &junk),
            ScanAction::Junk {
                signature: "Win.Test.EICAR".to_string()
            }
        );
        assert_eq!(
            decide_scan_action(&infected, &tag),
            ScanAction::Tag {
                signature: "Win.Test.EICAR".to_string()
            }
        );
    }

    #[test]
    fn scan_policy_default_matches_the_documented_defaults() {
        let policy = ScanPolicy::default();
        assert!(policy.clamav_enabled);
        assert_eq!(policy.clamav_action_on_infected, ClamavAction::Reject);
        assert!(policy.rspamd_enabled);
        assert_eq!(policy.rspamd_score_scaling_per_mille, 500);
    }
}
