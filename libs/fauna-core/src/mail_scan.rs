//! The perimeter content-scan results (ClamAV verdict, rspamd score) — **the one
//! definition**.
//!
//! These sit at the same three-way boundary as [`crate::mail_auth`], for the same
//! reason:
//!
//! - **Produced** by [`fauna_mail::scan`], the pure parser over clamd's and
//!   rspamd's replies, and exported over UniFFI so the Go MTA can build a scan
//!   result at the SMTP perimeter (`bins/fauna-bridges/internal/mta`).
//! - **Carried** on the `fauna.bridges.mail.ingest_inbound` WS-RPC request and
//!   re-exported by `fauna_protocol::bridge_routing`, the L3 transport crate.
//! - **Consumed** nest-side, flattened for storage
//!   (`bins/fauna-nest/src/bridge_routing_handlers.rs`, `message_scan_results`).
//!
//! Until 2026-08-18 the producer and the wire each carried a hand-mirrored copy —
//! the same duplication [`crate::mail_auth`] closed one family over, left behind only because each pair needed its own
//! wire-neutrality diff and its own Go call-site sweep. Both copies carried the
//! *same stated reason*, that `fauna-protocol` is the L3 wire crate and cannot
//! depend on `fauna-mail` (circular). **That premise was refuted by executing
//! it**: nothing circular was ever in the way, and the fix is not a move between
//! those two crates at all — it is a third home both already depend on.
//!
//! **`fauna-core` rather than `fauna-protocol`**, for the reason row 163
//! established: protocol carries no UniFFI surface at all, by design, and these
//! types must reach their Go producer. Here they need no new UniFFI face on the
//! transport crate and no new generated Go module.
//!
//! ## The two copies did NOT agree — and one of them was dead
//!
//! Unlike the auth verdicts, whose two copies matched byte-for-byte on every
//! serde attribute, these diverged:
//!
//! - `fauna-protocol`'s [`ClamavVerdict`] carried
//!   `#[serde(tag = "kind", content = "data", rename_all = "snake_case")]` and
//!   `Default`;
//! - `fauna-mail`'s carried **no serde container attributes at all** (so serde's
//!   default externally-tagged form) and no `Default`, but did carry the
//!   `uniffi::Enum` derive protocol's lacked;
//! - the two `Rspamd*` structs differed only in `Default` — identical fields in
//!   identical order, so genuinely wire-neutral.
//!
//! **The divergence was latent, never live.** `fauna-mail`'s serde impl is dead
//! on the wire: the Go MTA does not serialize the UniFFI type, it hand-builds the
//! adjacently-tagged shape (`internal/wsrpc/methods.go`'s
//! `ClamavVerdict{Kind, Data}`, fed by `mailfauna.ClamavVerdictToWire`), exactly
//! as it does for the auth family. Nothing in the Rust tree serialized the
//! `fauna-mail` copy either — its only uses were pattern matches and equality in
//! `libs/fauna-mail/tests/scan_tests.rs`. So the unified type takes
//! **protocol's** serde attributes (the wire truth) plus fauna-mail's UniFFI
//! derives plus `Default` — a strict union, and no byte on the wire moves.
//!
//! ## Wire shape — do not change without a major bump
//!
//! [`ClamavVerdict`] is **adjacently tagged**: `{"kind":"<variant>"}` for unit
//! variants, `{"kind":"<variant>","data":{…}}` for the two that carry payloads,
//! every name snake_case. That is what lets the Go mirror model it as a struct
//! with a `Kind string` plus an optional payload, and it is pinned across the
//! language boundary by
//! `internal/wsrpc/go_wire_variant_contract_test.go`'s sibling machinery.
//!
//! **No floats anywhere.** `serialization.md`'s strict dag-cbor decode rejects
//! them, so every rspamd score rides as a **milli-int** (`score * 1000`,
//! rounded), mirroring the FilterRule per-mille precedent.

use serde::{Deserialize, Serialize};

use crate::localized::LocalizedText;

/// ClamAV malware-scan verdict for one message.
///
/// Decoded by hand (below): a kind a newer build adds — unit or carrying
/// `data` — reads as [`Self::Unknown`]. The derive's `#[serde(other)]` cannot
/// do that on an adjacently tagged enum: it refuses an unknown kind that
/// carries a payload.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum ClamavVerdict {
    /// `stream: OK` — no signature matched. An affirmative claim about the
    /// message, so it is NOT the default: a verdict nobody computed is
    /// [`ClamavVerdict::NotScanned`].
    Clean,
    /// `stream: <signature> FOUND` — malware matched. Rides into the nest only
    /// on junk/tag actions; a `reject` action never reaches
    /// `ingest_inbound_mail`.
    Infected { signature: String },
    /// clamd returned an error (or an unrecognized reply). Must **NOT** be
    /// treated as clean — downstream this becomes a `Tempfail` (never
    /// allow-without-scan, per `mail-content-scanning.md` § Don't do these). A
    /// delivered message never carries this; it is present for the forensic
    /// report path.
    Error { detail: String },
    /// Message exceeded `clamav_max_filesize` and was delivered unscanned, with
    /// `X-Fauna-Scan-Clamav: bypassed_oversize`. The Go side produces this (it
    /// knows the size cap) without a clamd round-trip.
    BypassedOversize,
    /// ClamAV never ran for this message: it came through a door that does not
    /// invoke the scan gate (submission 465/587 — the colleague twin and the
    /// sender's own Sent copy — `mail-content-scanning.md` § Implementation
    /// status today, the per-door census) or the admin disabled ClamAV. Not a
    /// verdict: no `clamav` bus row, no `X-Fauna-Scan-Clamav` header, and the
    /// nest records `message_scan_results.clamav_verdict = 'not_scanned'` only
    /// when rspamd ran (otherwise the message never reached the scan pipeline
    /// and gets no row at all). Also the wire default — a request that says
    /// nothing about ClamAV did not scan.
    #[default]
    NotScanned,
    /// A verdict kind a newer build added that this one does not know — the
    /// app decodes this enum off `fauna.bridges.get_message_scan_result`'s
    /// reply, so an app older than its nest meets kinds it was not built
    /// with (`transport.md` § Schema and forward-compat discipline, rule 3:
    /// collapse — every reader only displays or scores the value, none
    /// re-emits it). Any payload is dropped. Never written: the bridge never
    /// produces it, the nest refuses it at ingest and never stores it, and
    /// it scores no row.
    #[serde(skip_serializing)]
    Unknown,
}

impl<'de> Deserialize<'de> for ClamavVerdict {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use fauna_cbor::Value;
        use serde::de::DeserializeOwned;

        #[derive(Deserialize)]
        struct Tagged {
            kind: String,
            #[serde(default)]
            data: Option<Value>,
        }
        #[derive(Deserialize)]
        struct InfectedData {
            signature: String,
        }
        #[derive(Deserialize)]
        struct ErrorData {
            detail: String,
        }
        fn payload<T: DeserializeOwned, E: serde::de::Error>(
            kind: &str,
            data: Option<Value>,
        ) -> Result<T, E> {
            let data =
                data.ok_or_else(|| E::custom(format!("clamav verdict {kind:?} carries no data")))?;
            let bytes = fauna_cbor::encode_canonical(&data).map_err(E::custom)?;
            fauna_cbor::decode_strict(&bytes).map_err(E::custom)
        }

        let Tagged { kind, data } = Tagged::deserialize(d)?;
        Ok(match kind.as_str() {
            "clean" => ClamavVerdict::Clean,
            "infected" => {
                let InfectedData { signature } = payload::<_, D::Error>(&kind, data)?;
                ClamavVerdict::Infected { signature }
            }
            "error" => {
                let ErrorData { detail } = payload::<_, D::Error>(&kind, data)?;
                ClamavVerdict::Error { detail }
            }
            "bypassed_oversize" => ClamavVerdict::BypassedOversize,
            "not_scanned" => ClamavVerdict::NotScanned,
            _ => ClamavVerdict::Unknown,
        })
    }
}

/// One rspamd symbol (rule) that fired, with its contribution as a milli-int
/// (signed: ham rules are negative).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct RspamdRuleContribution {
    pub rule: String,
    /// The rule's score contribution × 1000.
    pub score_milli: i32,
}

/// rspamd content-score result, all scores as milli-ints (no wire floats).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct RspamdScore {
    /// rspamd's native score × 1000 (native range ~0–30).
    pub raw_milli: i32,
    /// After applying `scaling_per_mille` (the value fed to mail-spam's 0–15
    /// scale) × 1000.
    pub scaled_milli: i32,
    /// Names of every symbol that fired, sorted (JSON map order is unstable).
    pub flagged_rules: Vec<String>,
    /// Per-rule contributions, sorted by rule name.
    pub breakdown: Vec<RspamdRuleContribution>,
}

/// What the nest did with a scanned message — the `message_scan_results.
/// action_taken` column as a type (`mail-content-scanning.md` § Per-message
/// scan-result storage). `RejectedMalware` appears only on reject-at-perimeter
/// forensic rows, which belong to no recipient, so a user's own read never
/// returns it.
///
/// Here rather than in `fauna_protocol::bridge_routing` (which re-exports it)
/// so [`scan_result_lines`] — the app-facing projection every shell paints —
/// can take it without the L3 transport crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanActionTaken {
    Delivered,
    RejectedMalware,
    Junked,
    Tagged,
    /// An action a newer nest added that this app does not know: shown as a
    /// neutral verdict. Never written.
    #[serde(other, skip_serializing)]
    Unknown,
}

impl ScanActionTaken {
    /// The column spelling — identical to the serde name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Delivered => "delivered",
            Self::RejectedMalware => "rejected_malware",
            Self::Junked => "junked",
            Self::Tagged => "tagged",
            Self::Unknown => "unknown",
        }
    }

    /// Parse the column spelling; `None` for anything else.
    pub fn from_column(s: &str) -> Option<Self> {
        [
            Self::Delivered,
            Self::RejectedMalware,
            Self::Junked,
            Self::Tagged,
        ]
        .into_iter()
        .find(|a| a.as_str() == s)
    }
}

/// The lines a user's received mail message shows about its own perimeter
/// scan — the `dm-message-scan-result` row on the message bubble
/// (`mail-content-scanning.md` § User sees their own message's row): the
/// ClamAV verdict, the rspamd score with the rules that fired, and what the
/// nest did with the message, in that order. Keys, not English, so every app
/// resolves them through its own i18n pipeline and no app re-derives the
/// wording (the shared `conversations.scan_result.*` strings).
///
/// The `Delivered to:` line the goal's example opens with is NOT here: the
/// bubble's own `dm-message-delivered-to-alias` paints it.
pub fn scan_result_lines(
    clamav: &ClamavVerdict,
    rspamd: Option<&RspamdScore>,
    action: ScanActionTaken,
) -> Vec<LocalizedText> {
    let clamav = match clamav {
        ClamavVerdict::Clean => LocalizedText::key("conversations.scan_result.clamav_clean"),
        ClamavVerdict::Infected { signature } => LocalizedText::key_arg(
            "conversations.scan_result.clamav_infected",
            "signature",
            signature,
        ),
        ClamavVerdict::Error { .. } => LocalizedText::key("conversations.scan_result.clamav_error"),
        ClamavVerdict::BypassedOversize => {
            LocalizedText::key("conversations.scan_result.clamav_bypassed_oversize")
        }
        ClamavVerdict::NotScanned => {
            LocalizedText::key("conversations.scan_result.clamav_not_scanned")
        }
        ClamavVerdict::Unknown => LocalizedText::key("conversations.scan_result.clamav_unknown"),
    };
    let rspamd = match rspamd {
        None => LocalizedText::key("conversations.scan_result.rspamd_not_scanned"),
        Some(score) if score.flagged_rules.is_empty() => LocalizedText::key_arg(
            "conversations.scan_result.rspamd_score",
            "score",
            milli_to_decimal(score.raw_milli),
        ),
        Some(score) => {
            let mut text = LocalizedText::key_arg(
                "conversations.scan_result.rspamd_score_rules",
                "score",
                milli_to_decimal(score.raw_milli),
            );
            text.args
                .insert("rules".into(), score.flagged_rules.join(", "));
            text
        }
    };
    let action = LocalizedText::key(match action {
        ScanActionTaken::Delivered => "conversations.scan_result.action_delivered",
        ScanActionTaken::RejectedMalware => "conversations.scan_result.action_rejected_malware",
        ScanActionTaken::Junked => "conversations.scan_result.action_junked",
        ScanActionTaken::Tagged => "conversations.scan_result.action_tagged",
        ScanActionTaken::Unknown => "conversations.scan_result.action_unknown",
    });
    vec![clamav, rspamd, action]
}

/// A milli-int score as the decimal rspamd prints, one place: `1200` → `1.2`,
/// `-450` → `-0.5` (rounded half away from zero), `0` → `0.0`.
fn milli_to_decimal(milli: i32) -> String {
    let tenths = (i64::from(milli) * 10 + if milli < 0 { -500 } else { 500 }) / 1000;
    let sign = if tenths < 0 { "-" } else { "" };
    let tenths = tenths.abs();
    format!("{sign}{}.{}", tenths / 10, tenths % 10)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_result_lines_are_clamav_then_rspamd_then_action() {
        let score = RspamdScore {
            raw_milli: 1200,
            flagged_rules: vec!["BAYES_HAM".into(), "MIME_GOOD".into()],
            ..Default::default()
        };
        let lines = scan_result_lines(
            &ClamavVerdict::Clean,
            Some(&score),
            ScanActionTaken::Delivered,
        );
        let keys: Vec<&str> = lines.iter().map(|l| l.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "conversations.scan_result.clamav_clean",
                "conversations.scan_result.rspamd_score_rules",
                "conversations.scan_result.action_delivered",
            ]
        );
        assert_eq!(lines[1].args["score"], "1.2");
        assert_eq!(lines[1].args["rules"], "BAYES_HAM, MIME_GOOD");
    }

    #[test]
    fn scan_result_lines_name_the_signature_and_a_missing_rspamd_run() {
        let lines = scan_result_lines(
            &ClamavVerdict::Infected {
                signature: "Eicar-Test-Signature".into(),
            },
            None,
            ScanActionTaken::Junked,
        );
        assert_eq!(lines[0].key, "conversations.scan_result.clamav_infected");
        assert_eq!(lines[0].args["signature"], "Eicar-Test-Signature");
        assert_eq!(lines[1].key, "conversations.scan_result.rspamd_not_scanned");
        assert_eq!(lines[2].key, "conversations.scan_result.action_junked");
    }

    #[test]
    fn milli_scores_print_as_rspamd_does() {
        for (milli, want) in [
            (1200, "1.2"),
            (0, "0.0"),
            (-450, "-0.5"),
            (15_049, "15.0"),
            (15_050, "15.1"),
            (-3000, "-3.0"),
        ] {
            assert_eq!(milli_to_decimal(milli), want, "{milli}");
        }
    }

    fn known() -> Vec<ClamavVerdict> {
        vec![
            ClamavVerdict::Clean,
            ClamavVerdict::Infected {
                signature: "Eicar-Test-Signature".into(),
            },
            ClamavVerdict::Error {
                detail: "clamd timeout".into(),
            },
            ClamavVerdict::BypassedOversize,
            ClamavVerdict::NotScanned,
        ]
    }

    #[test]
    fn hand_written_decode_round_trips_every_known_kind() {
        for v in known() {
            let bytes = fauna_cbor::encode_canonical(&v).unwrap();
            assert_eq!(
                v,
                fauna_cbor::decode_strict::<ClamavVerdict>(&bytes).unwrap()
            );
            let json = serde_json::to_string(&v).unwrap();
            assert_eq!(v, serde_json::from_str::<ClamavVerdict>(&json).unwrap());
        }
    }

    #[test]
    fn an_unknown_kind_reads_as_unknown_with_or_without_data() {
        for json in [
            r#"{"kind":"quarantined","data":{"vault":"v1","n":3}}"#,
            r#"{"kind":"deferred"}"#,
            r#"{"kind":"deferred","data":7}"#,
        ] {
            assert_eq!(
                serde_json::from_str::<ClamavVerdict>(json).unwrap(),
                ClamavVerdict::Unknown
            );
        }
    }

    #[test]
    fn a_known_kind_missing_its_data_is_refused() {
        assert!(serde_json::from_str::<ClamavVerdict>(r#"{"kind":"infected"}"#).is_err());
    }

    #[test]
    fn unknown_is_never_written() {
        assert!(fauna_cbor::encode_canonical(&ClamavVerdict::Unknown).is_err());
        assert!(serde_json::to_string(&ClamavVerdict::Unknown).is_err());
    }
}
