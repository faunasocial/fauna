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

/// ClamAV malware-scan verdict for one message.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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
