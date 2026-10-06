//! User-facing WS-RPC payload types for the moderation surface —
//! `fauna.moderation.{stats,actions,appeal,train}` and the report/signal kinds.
//! A faithful transport migration of the former moderation HTTP routes; no
//! moderation route stays HTTP.
//!
//! Confidence scores ride as **per-mille** `u16` (0–1000 = probability
//! `[0.0, 1.0]` × 1000): the dag-cbor wire forbids floats
//! (`docs/goal/architecture/serialization.md` § Floats). This mirrors the
//! spam-preferences threshold encoding (`fauna_protocol::spam`).
//!
//! Kind metadata lives in `kind.rs::register_moderation_kinds`.
//! Slice tracked internally as part of the WS-RPC-everywhere migration.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use fauna_cbor::Value;

// ── fauna.moderation.stats ─────────────────────────────────────────────

/// Read aggregate label statistics. Empty request — nest-wide aggregate,
/// no per-actor scope (the HTTP twin took no auth).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ModerationStatsRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One category's aggregate counts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct LabelStat {
    pub category: String,
    pub count: i64,
    /// Average classifier confidence as per-mille (0–1000).
    pub avg_confidence_per_mille: u16,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ModerationStatsReply {
    pub labels: Vec<LabelStat>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.moderation.actions ───────────────────────────────────────────

/// Read obligation-action records for the **connection actor's own** content.
/// Empty request — the WS-RPC connection knows its caller. The HTTP twin's
/// `?actor={hex}` query (which let any caller read any actor's records) is
/// dropped: the connection actor is always the subject.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ModerationActionsRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One obligation-action record — why a piece of the caller's content was
/// rejected / quarantined / labeled.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ObligationAction {
    pub id: i64,
    pub content_type: String,
    pub content_id: String,
    pub category: String,
    /// Classifier confidence as per-mille (0–1000).
    pub confidence_per_mille: u16,
    /// Action taken (raw obligation-action discriminant, as stored).
    pub action: u8,
    /// Microsecond timestamp.
    pub timestamp: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ModerationActionsReply {
    pub actions: Vec<ObligationAction>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.moderation.appeal ────────────────────────────────────────────

/// The longest appeal `reason` the nest records, in UTF-8 bytes — a human
/// paragraph or three, not a document. The appeal lands on the permanent,
/// un-prunable audit chain, so the bound is what keeps one caller from growing
/// that chain by a transport frame per call (`moderation.md` § Errors & edge
/// cases). The nest refuses a longer reason with `invalid_params` before any
/// write; the shared `fauna_client_moderation::appeal_form_view` renders the
/// same bound before dispatch — one constant, so the two can never disagree.
pub const MAX_APPEAL_REASON_BYTES: usize = 4096;

/// Appeal an enforcement decision recorded against `content_id` — the second
/// leg of the transparency triple (`moderation.md` § Legal takedown). Today
/// production only ever records legal takedowns (post *and* conversation), so
/// that is what is appealed; a mail quarantine/reject row would be appealed
/// through the same kind the day one is written. The nest refuses a
/// `content_id` it holds no enforcement record for (`not_found`), a post appeal
/// from anyone but the post's author (`permission_denied`) and a `reason` over
/// [`MAX_APPEAL_REASON_BYTES`] (`invalid_params`), and logs the rest to the
/// permanent audit trail for admin review — recorded, not granted. A repeat
/// while the caller's appeal on the content is still pending writes nothing and
/// replies `status: "appeal_already_recorded"` (`moderation.md` § Errors &
/// edge cases).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ModerationAppealRequest {
    pub content_id: String,
    pub reason: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ModerationAppealReply {
    pub status: String,
    pub content_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.moderation.train ─────────────────────────────────────────────

/// Record the connection actor's spam/ham verdict on a stored post — the nest
/// half of a training correction. The nest no longer trains: it checks the
/// caller may read the post and captures the opt-in report
/// (`report-sharing.md` § Report capture). The model half is the client's own
/// sealed write (`fauna.bridges.put_spam_model`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ModerationTrainRequest {
    /// Hex-encoded 32-byte post id the verdict is about.
    pub content_id: String,
    /// `"spam"` or `"ham"`.
    pub verdict: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ModerationTrainReply {
    pub status: String,
    pub verdict: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.moderation.legal_takedown ────────────────────────────────────

fn default_post_content_type() -> String {
    "post".to_string()
}

/// Admin-initiated **legal-compulsion** takedown of social content — the
/// narrow, transparent, appealable carve-out (`moderation.md` § Categories &
/// enforcement item 1 / `content-moderation-and-ranking.md` § Resolved design
/// decisions Q5). This is the **one** compulsory-social-removal case: genuinely
/// illegal content (e.g. CSAM, a court-ordered takedown).
///
/// It is **structurally incapable of being a general "remove for policy" lever**:
/// `restore == false` **requires a non-empty `legal_reference`**, the takedown
/// produces a **visible tombstone** ("removed under legal obligation
/// [reference]") shown in place of the body, an `obligation_action_records` row
/// (`ObligationAction::TakenDown`), and an audit row, and it is **appealable**
/// (`fauna.moderation.appeal`). It is **never** a silent removal. `restore ==
/// true` overturns an upheld appeal — it clears the takedown (the content is
/// tombstoned, never hard-deleted, so restore is always possible) and records
/// the reason; `legal_reference` is then the optional overturn note.
///
/// Admin-class only (`bridge_method_allowlist`): the admin is the deployment's
/// legal-compliance responder (there is no operator). The four structural
/// guards above — not the caller class — are what keep it non-discretionary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModerationLegalTakedownRequest {
    /// Hex-encoded 32-byte content id of the post to take down / restore.
    pub content_id: String,
    /// Content kind — `"post"` today (default). Reserved for future social
    /// kinds; the nest rejects anything it cannot withhold at serve time.
    #[serde(default = "default_post_content_type")]
    pub content_type: String,
    /// The legal-obligation reference the takedown cites (a court order id, a
    /// statutory reference, …). **Required** when `restore == false`; the
    /// optional overturn note when `restore == true`.
    #[serde(default)]
    pub legal_reference: String,
    /// `false` (default) = take down; `true` = overturn/restore.
    #[serde(default)]
    pub restore: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

impl Default for ModerationLegalTakedownRequest {
    fn default() -> Self {
        Self {
            content_id: String::new(),
            content_type: default_post_content_type(),
            legal_reference: String::new(),
            restore: false,
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ModerationLegalTakedownReply {
    /// `"taken_down"` or `"restored"`.
    pub status: String,
    pub content_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.moderation.report_share.{set,status} ─────────────────────────
//
// The distributed-report-sharing opt-in + transparency surface
// (`report-sharing.md` § Client wire + transparency surface). Both are
// **caller-scoped** — the connection actor is always the subject; neither
// carries an `actor_id`, so even an admin sets/reads only their own opt-in.
// (The `published` list is nest-wide by design — see `ReportShareEntry`.)

/// Set the connection actor's report-sharing opt-in
/// (`spam_preferences.share_reports`, default off). `share=false` also
/// deletes every report row this actor contributed and recomputes each
/// affected aggregate (user-controls-their-data — a withdrawn judgment
/// leaves no residue).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ModerationReportShareSetRequest {
    pub share: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ModerationReportShareSetReply {
    /// The opt-in state now in effect (echoes the request).
    pub share: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Read the connection actor's opt-in state **and** exactly what this nest
/// publishes to the world — empty request (the connection knows its caller).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ModerationReportShareStatusRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One published aggregate — a `(content_hash, factor, count)` triple that
/// has passed the k-anonymity gate. This is the same shape the federation
/// **export** serves a peer nest (`report-sharing.md` § Federation exchange);
/// `count` is always a local reporter count ≥ k (never a below-k count, never
/// a reporter identity).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ReportShareEntry {
    /// Hex-encoded 32-byte report-hash.
    pub content_hash: String,
    /// Scoring factor (`"report:spam"` today).
    pub factor: String,
    /// Distinct local reporter count (≥ k).
    pub count: u32,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The opt-in state + the transparency list. `published` is **byte-identical
/// to the federation export** — the same ≥k list a peer nest would receive.
/// That identity is the transparency guarantee: the user sees precisely what
/// their nest tells the world, nothing hidden, nothing extra. It is
/// **nest-wide** (every ≥k aggregate on this nest), not caller-scoped — the
/// point is to show what the nest exports, and reporter identity never
/// appears at any count.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ModerationReportShareStatusReply {
    pub share: bool,
    pub published: Vec<ReportShareEntry>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.moderation.signal_share.{set,status} + signal_contribute ─────
//
// Layer-B engagement-cue contribution (`engagement-cues.md` § Layer B nest
// legs) — the opt-in sibling of report sharing. `signal_share.{set,status}`
// mirror `report_share.{set,status}` verbatim (caller-scoped opt-in +
// nest-wide transparency export view), for the INDEPENDENT `share_signals`
// preference; `signal_contribute` is the write path a client calls when its
// `CueEngine` derives a shareable per-item verdict. All ride the SAME
// `content_reports` table / k-gate / count curve / federation exchange as
// `report:spam` — no new tables, no new federation kinds.

/// Set the connection actor's engagement-signal-sharing opt-in
/// (`spam_preferences.share_signals`, default off). Caller-scoped (no
/// `actor_id` on the wire — even an admin sets only their own). `share=false`
/// deletes every `signal:*` row this actor contributed and recomputes each
/// affected aggregate — INDEPENDENT of `report_share` (never touches the
/// actor's `report:spam` rows).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ModerationSignalShareSetRequest {
    pub share: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ModerationSignalShareSetReply {
    /// The opt-in state now in effect (echoes the request).
    pub share: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Read the connection actor's signal opt-in state + exactly what this nest
/// publishes — empty request (the connection knows its caller).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ModerationSignalShareStatusRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The signal opt-in state + the transparency list. `published` is **the same
/// federation export view** as `report_share.status` (byte-identical — one
/// export function), so it carries every ≥k aggregate this nest publishes,
/// both `report:*` and `signal:*` (the nest tells the world both). Reuses
/// [`ReportShareEntry`] (the shared `(content_hash, factor, count)` shape).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ModerationSignalShareStatusReply {
    pub share: bool,
    pub published: Vec<ReportShareEntry>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Contribute one derived engagement-cue verdict for a **public** post
/// (`engagement-cues.md` § Layer B — the write path). Caller-scoped;
/// honored only when the actor opted in (`signal_share.set{share:true}`),
/// except `withdraw` which always applies (a retraction). `content_id` is the
/// hex 32-byte post id (a post's content-addressed id IS its report-hash).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ModerationSignalContributeRequest {
    /// Hex-encoded 32-byte post id the verdict is about.
    pub content_id: String,
    /// `"watch-complete"`, `"skip"`, or `"withdraw"` (last-wins per item;
    /// a flip withdraws the old factor and inserts the new).
    pub signal: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ModerationSignalContributeReply {
    pub status: String,
    /// The verdict recorded (echoes the request).
    pub signal: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.moderation.abuse_report.* ────────────────────────────────────
//
// User-initiated reporting (`moderation.md` § User-initiated reporting): a
// report is **evidence to a human with authority**, never a lever. It lands on
// the reporter's own nest admin's queue and is forwarded, reporter-anonymously,
// to the reported author's home nest when that author is foreign
// (`fauna.federation.abuse_report.*`, below). Nothing here writes the
// k-anonymized report aggregates or any content label.

/// The longest reporter note the nest records, in UTF-8 bytes. The nest refuses
/// a longer note with `invalid_params` before any write; the shared
/// `fauna_client_moderation::report` sheet view renders the same bound before
/// dispatch — one constant, so the two can never disagree.
pub const MAX_ABUSE_REPORT_NOTE_BYTES: usize = 2000;

/// The longest reporter-attached excerpt of a **sealed** subject, in UTF-8
/// bytes (`moderation.md` § What a report carries). Refused over the bound
/// exactly as the note is.
pub const MAX_ABUSE_REPORT_EXCERPT_BYTES: usize = 4096;

/// What a report is about. Internally tagged on `kind`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AbuseReportSubject {
    /// A post, by its hex-encoded 32-byte content id.
    Post { cid: String },
    /// A conversation message — the same `(channel, record_cid)` key the
    /// conversation half of § Legal takedown withholds by, so an admin can act
    /// on a sealed message without ever reading it.
    Message { channel: String, record_cid: String },
    /// An account (a profile report), by its hex actor id.
    Actor { actor_id: String },
    /// A subject kind a newer build added that this build cannot read
    /// (`transport.md` § Schema and forward-compat discipline, rule 3: open,
    /// carrying). Carried so a reply that lists the report re-emits it; it is
    /// never shown as a known kind, offers no action, and a nest refuses to
    /// record a new report about it.
    #[serde(untagged)]
    Unknown(fauna_core::carried::CarriedValue),
}

impl AbuseReportSubject {
    /// The stored `subject_kind` discriminant. `"unknown"` for
    /// [`AbuseReportSubject::Unknown`], which no nest stores.
    pub fn kind(&self) -> &'static str {
        match self {
            AbuseReportSubject::Post { .. } => "post",
            AbuseReportSubject::Message { .. } => "message",
            AbuseReportSubject::Actor { .. } => "actor",
            AbuseReportSubject::Unknown(_) => "unknown",
        }
    }

    /// The stored `subject_id` — the key the one-open-per-(reporter, subject)
    /// dedupe and the doorbell's per-subject dedupe bind on. A message is keyed
    /// by its record cid (unique across channels). Empty for
    /// [`AbuseReportSubject::Unknown`], which no nest stores.
    pub fn id(&self) -> &str {
        match self {
            AbuseReportSubject::Post { cid } => cid,
            AbuseReportSubject::Message { record_cid, .. } => record_cid,
            AbuseReportSubject::Actor { actor_id } => actor_id,
            AbuseReportSubject::Unknown(_) => "",
        }
    }

    /// Whether the nest holds no readable bytes for this subject kind, so an
    /// admin can judge the text only through a reporter-attached excerpt. A
    /// conversation message is always sealed; a post's sealed-ness is the
    /// client's to know (a gated post), so the post arm answers `false` and the
    /// sheet view takes the client's word for a gated post.
    pub fn is_always_sealed(&self) -> bool {
        matches!(self, AbuseReportSubject::Message { .. })
    }
}

/// The report-reason vocabulary — deliberately distinct from the canonical-5
/// content-label categories (those are classifier emissions; a report emits no
/// label). A reason a newer app adds decodes as [`AbuseReportReason::Other`] on
/// an older nest, so the report still lands rather than bouncing.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AbuseReportReason {
    Spam,
    Harassment,
    Hate,
    Violence,
    Sexual,
    Illegal,
    Impersonation,
    #[serde(other)]
    Other,
}

impl AbuseReportReason {
    /// Every reason, in the order the report sheet offers them.
    pub const ALL: [AbuseReportReason; 8] = [
        AbuseReportReason::Spam,
        AbuseReportReason::Harassment,
        AbuseReportReason::Hate,
        AbuseReportReason::Violence,
        AbuseReportReason::Sexual,
        AbuseReportReason::Illegal,
        AbuseReportReason::Impersonation,
        AbuseReportReason::Other,
    ];

    /// The stored / i18n-key token (`moderation.report.reason.<token>`).
    pub fn token(self) -> &'static str {
        match self {
            AbuseReportReason::Spam => "spam",
            AbuseReportReason::Harassment => "harassment",
            AbuseReportReason::Hate => "hate",
            AbuseReportReason::Violence => "violence",
            AbuseReportReason::Sexual => "sexual",
            AbuseReportReason::Illegal => "illegal",
            AbuseReportReason::Impersonation => "impersonation",
            AbuseReportReason::Other => "other",
        }
    }

    /// Parse a stored token; anything unrecognised reads as `Other`.
    pub fn from_token(token: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|r| r.token() == token)
            .unwrap_or(AbuseReportReason::Other)
    }
}

/// A report's lifecycle state.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AbuseReportStatus {
    Open,
    Resolved,
    Withdrawn,
    /// A state a newer nest added that this build does not know. Never
    /// serialized by this build.
    #[serde(other)]
    Unknown,
}

impl AbuseReportStatus {
    pub fn token(self) -> &'static str {
        match self {
            AbuseReportStatus::Open => "open",
            AbuseReportStatus::Resolved => "resolved",
            AbuseReportStatus::Withdrawn => "withdrawn",
            AbuseReportStatus::Unknown => "unknown",
        }
    }

    pub fn from_token(token: &str) -> Self {
        match token {
            "open" => AbuseReportStatus::Open,
            "resolved" => AbuseReportStatus::Resolved,
            "withdrawn" => AbuseReportStatus::Withdrawn,
            _ => AbuseReportStatus::Unknown,
        }
    }
}

/// An admin's resolution — a **record, not an action**: resolving does nothing
/// to the content or the author (`moderation.md` § Where it lands).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AbuseReportOutcome {
    Acted,
    Dismissed,
    /// An outcome a newer nest added that this build does not know. The nest
    /// refuses it on `resolve`; never serialized by this build.
    #[serde(other)]
    Unknown,
}

impl AbuseReportOutcome {
    pub fn token(self) -> &'static str {
        match self {
            AbuseReportOutcome::Acted => "acted",
            AbuseReportOutcome::Dismissed => "dismissed",
            AbuseReportOutcome::Unknown => "unknown",
        }
    }

    pub fn from_token(token: &str) -> Self {
        match token {
            "acted" => AbuseReportOutcome::Acted,
            "dismissed" => AbuseReportOutcome::Dismissed,
            _ => AbuseReportOutcome::Unknown,
        }
    }
}

/// `fauna.moderation.abuse_report.submit` — file a report. Caller-scoped (the
/// connection actor is the reporter). The nest refuses an empty subject id, an
/// over-bound note or excerpt (`invalid_params`) and a caller past
/// `ABUSE_REPORTS_PER_HOUR` (`rate_limited`); a second report on a subject the
/// caller already has an open report on writes nothing and replies with the
/// existing report (replay-safe).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AbuseReportSubmitRequest {
    pub subject: AbuseReportSubject,
    pub reason: AbuseReportReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Reporter-attached plaintext of a sealed subject — opt-in per report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
    /// Chain the existing `fauna.knocks.block` on the subject's author. The
    /// client performs the block through its own seam; the flag is recorded so
    /// the reporter's ledger can say so.
    #[serde(default)]
    pub block_author: bool,
    /// The subject's author (hex actor id), as the reporting client holds it —
    /// what routes the report to the author's home nest. The nest takes its
    /// own record over this wherever it has one (a stored post's author, an
    /// actor subject's own id); for a sealed conversation message, whose sender
    /// the nest never learns, the client's word is the only source. Naming the
    /// wrong author gains nothing an `actor` report on that author would not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_actor: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AbuseReportSubmitReply {
    /// Opaque report id (hex) — also the federation `report_ref`.
    pub report_id: String,
    /// The nest domains the report reached: this nest first, then the author's
    /// home nest when forwarded.
    pub routed_to: Vec<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.moderation.abuse_report.mine` — the caller's own reports (the
/// Moderation page's *Your reports* ledger). Empty request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AbuseReportMineRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One row of the reporter's ledger.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AbuseReportMineEntry {
    pub report_id: String,
    pub subject: AbuseReportSubject,
    pub reason: AbuseReportReason,
    /// Microsecond timestamp.
    pub created_at: i64,
    pub status: AbuseReportStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<AbuseReportOutcome>,
    pub routed_to: Vec<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AbuseReportMineReply {
    pub reports: Vec<AbuseReportMineEntry>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.moderation.abuse_report.withdraw` — withdraw one of the caller's
/// **open** reports. Deletes the note and excerpt here and on every nest the
/// report was forwarded to. Idempotent on an already-withdrawn report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AbuseReportWithdrawRequest {
    pub report_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AbuseReportWithdrawReply {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.moderation.abuse_report.queue` — Admin: the open reports on this
/// nest, local and forwarded. Empty request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AbuseReportQueueRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One row of the admin queue.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AbuseReportQueueEntry {
    pub report_id: String,
    pub subject: AbuseReportSubject,
    /// The subject's author (hex actor id), when this nest could resolve it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_actor: Option<String>,
    pub reason: AbuseReportReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
    /// A local reporter's handle. `None` on a forwarded report — the reporter's
    /// identity never crosses a nest boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reporter_handle: Option<String>,
    /// The forwarding nest's domain, on a forwarded report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_nest: Option<String>,
    /// Microsecond timestamp.
    pub created_at: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AbuseReportQueueReply {
    pub reports: Vec<AbuseReportQueueEntry>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.moderation.abuse_report.resolve` — Admin: record an open report's
/// outcome. A record only; it writes the reporter's notification (or, for a
/// forwarded report, the federation outcome back to the origin nest).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AbuseReportResolveRequest {
    pub report_id: String,
    pub outcome: AbuseReportOutcome,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AbuseReportResolveReply {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.federation.abuse_report.{deliver,withdraw,outcome} ───────────
//
// The forwarding triad (`moderation.md` § Routing). Nest-signature attributed;
// **no reporter identity** on any leg. `report_ref` is the origin nest's
// `report_id`.

/// Forward a report to the subject author's home nest. The receiver accepts it
/// only for a subject it hosts, and throttles per origin nest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AbuseReportDeliverRequest {
    pub report_ref: String,
    pub subject: AbuseReportSubject,
    pub reason: AbuseReportReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
    /// The origin nest's domain, shown to the receiving admin as "a user of
    /// <nest>".
    pub origin_nest_id: String,
    /// The origin nest's own URL (`https://<domain>`), declared so the
    /// receiver can return the outcome to a nest it may never have dialled.
    /// Peer-declared: the receiver honours it only against the channel's
    /// verified origin (a dial-proven address wins), exactly as a relayed
    /// Welcome's declared origin is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_nest_url: Option<String>,
    /// The subject's author as the origin nest resolved it — the receiver
    /// checks it names an account it hosts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_actor: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Withdraw a forwarded report: the receiver marks it withdrawn and deletes
/// its note and excerpt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AbuseReportFederationWithdrawRequest {
    pub report_ref: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Return a forwarded report's outcome to its origin nest, which writes the
/// reporter's notification.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AbuseReportFederationOutcomeRequest {
    pub report_ref: String,
    pub outcome: AbuseReportOutcome,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The empty ack every triad leg replies with.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AbuseReportFederationAck {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict as decode, encode_canonical};

    fn re_encodes_identically<T>(v: &T)
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
    {
        let bytes1 = encode_canonical(v).unwrap();
        let decoded: T = decode(&bytes1).unwrap();
        assert_eq!(v, &decoded);
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2, "canonical re-encode must be byte-identical");
    }

    #[test]
    fn stats_round_trips() {
        re_encodes_identically(&ModerationStatsRequest::default());
        re_encodes_identically(&ModerationStatsReply {
            labels: vec![
                LabelStat {
                    category: "spam".into(),
                    count: 7,
                    avg_confidence_per_mille: 925,
                    extra: BTreeMap::new(),
                },
                LabelStat {
                    category: "phishing".into(),
                    count: 2,
                    avg_confidence_per_mille: 600,
                    extra: BTreeMap::new(),
                },
            ],
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn actions_round_trips() {
        re_encodes_identically(&ModerationActionsRequest::default());
        re_encodes_identically(&ModerationActionsReply {
            actions: vec![ObligationAction {
                id: 42,
                content_type: "post".into(),
                content_id: "ab".repeat(32),
                category: "spam".into(),
                confidence_per_mille: 880,
                action: 2,
                timestamp: 1_700_000_000_000_000,
                extra: BTreeMap::new(),
            }],
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn appeal_round_trips() {
        re_encodes_identically(&ModerationAppealRequest {
            content_id: "cd".repeat(32),
            reason: "false positive".into(),
            extra: BTreeMap::new(),
        });
        re_encodes_identically(&ModerationAppealReply {
            status: "appeal_recorded".into(),
            content_id: "cd".repeat(32),
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn train_round_trips() {
        re_encodes_identically(&ModerationTrainRequest {
            content_id: "12".repeat(32),
            verdict: "spam".into(),
            extra: BTreeMap::new(),
        });
        re_encodes_identically(&ModerationTrainReply {
            status: "trained".into(),
            verdict: "ham".into(),
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn legal_takedown_round_trips() {
        re_encodes_identically(&ModerationLegalTakedownRequest {
            content_id: "ab".repeat(32),
            content_type: "post".into(),
            legal_reference: "EU-DSA-2024/12345".into(),
            restore: false,
            extra: BTreeMap::new(),
        });
        // Restore form (overturn) — no legal_reference required on the wire.
        re_encodes_identically(&ModerationLegalTakedownRequest {
            content_id: "cd".repeat(32),
            content_type: "post".into(),
            legal_reference: String::new(),
            restore: true,
            extra: BTreeMap::new(),
        });
        re_encodes_identically(&ModerationLegalTakedownReply {
            status: "taken_down".into(),
            content_id: "ab".repeat(32),
            extra: BTreeMap::new(),
        });
        // content_type defaults to "post" when omitted.
        let bytes = encode_canonical(&ModerationLegalTakedownReply::default()).unwrap();
        let _: ModerationLegalTakedownReply = decode(&bytes).unwrap();
        assert_eq!(
            ModerationLegalTakedownRequest::default().content_type,
            "post"
        );
    }

    #[test]
    fn report_share_set_round_trips() {
        re_encodes_identically(&ModerationReportShareSetRequest {
            share: true,
            extra: BTreeMap::new(),
        });
        re_encodes_identically(&ModerationReportShareSetRequest::default());
        re_encodes_identically(&ModerationReportShareSetReply {
            share: false,
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn report_share_status_round_trips() {
        re_encodes_identically(&ModerationReportShareStatusRequest::default());
        re_encodes_identically(&ModerationReportShareStatusReply {
            share: true,
            published: vec![
                ReportShareEntry {
                    content_hash: "ab".repeat(32),
                    factor: "report:spam".into(),
                    count: 7,
                    extra: BTreeMap::new(),
                },
                ReportShareEntry {
                    content_hash: "cd".repeat(32),
                    factor: "report:spam".into(),
                    count: 3,
                    extra: BTreeMap::new(),
                },
            ],
            extra: BTreeMap::new(),
        });
        // Empty published list (the common fresh-nest case).
        re_encodes_identically(&ModerationReportShareStatusReply::default());
    }

    #[test]
    fn signal_share_round_trips() {
        re_encodes_identically(&ModerationSignalShareSetRequest {
            share: true,
            extra: BTreeMap::new(),
        });
        re_encodes_identically(&ModerationSignalShareSetRequest::default());
        re_encodes_identically(&ModerationSignalShareSetReply {
            share: false,
            extra: BTreeMap::new(),
        });
        re_encodes_identically(&ModerationSignalShareStatusRequest::default());
        re_encodes_identically(&ModerationSignalShareStatusReply {
            share: true,
            // The export view carries both factor families.
            published: vec![
                ReportShareEntry {
                    content_hash: "ab".repeat(32),
                    factor: "signal:watch-complete".into(),
                    count: 5,
                    extra: BTreeMap::new(),
                },
                ReportShareEntry {
                    content_hash: "cd".repeat(32),
                    factor: "report:spam".into(),
                    count: 3,
                    extra: BTreeMap::new(),
                },
            ],
            extra: BTreeMap::new(),
        });
        re_encodes_identically(&ModerationSignalShareStatusReply::default());
    }

    fn subjects() -> Vec<AbuseReportSubject> {
        vec![
            AbuseReportSubject::Post {
                cid: "ab".repeat(32),
            },
            AbuseReportSubject::Message {
                channel: "cd".repeat(16),
                record_cid: "bafyrecord".into(),
            },
            AbuseReportSubject::Actor {
                actor_id: "ef".repeat(32),
            },
        ]
    }

    #[test]
    fn abuse_report_user_kinds_round_trip() {
        for subject in subjects() {
            re_encodes_identically(&AbuseReportSubmitRequest {
                subject: subject.clone(),
                reason: AbuseReportReason::Harassment,
                note: Some("repeated slurs".into()),
                excerpt: None,
                block_author: true,
                subject_actor: None,
                extra: BTreeMap::new(),
            });
            re_encodes_identically(&AbuseReportMineReply {
                reports: vec![AbuseReportMineEntry {
                    report_id: "0f".repeat(16),
                    subject: subject.clone(),
                    reason: AbuseReportReason::Spam,
                    created_at: 1_700_000_000_000_000,
                    status: AbuseReportStatus::Resolved,
                    outcome: Some(AbuseReportOutcome::Acted),
                    routed_to: vec!["a.example".into(), "b.example".into()],
                    extra: BTreeMap::new(),
                }],
                extra: BTreeMap::new(),
            });
            re_encodes_identically(&AbuseReportQueueReply {
                reports: vec![AbuseReportQueueEntry {
                    report_id: "0f".repeat(16),
                    subject,
                    subject_actor: Some("ef".repeat(32)),
                    reason: AbuseReportReason::Other,
                    note: None,
                    excerpt: Some("the text".into()),
                    reporter_handle: None,
                    origin_nest: Some("b.example".into()),
                    created_at: 1,
                    extra: BTreeMap::new(),
                }],
                extra: BTreeMap::new(),
            });
        }
        re_encodes_identically(&AbuseReportSubmitReply {
            report_id: "0f".repeat(16),
            routed_to: vec!["a.example".into()],
            extra: BTreeMap::new(),
        });
        re_encodes_identically(&AbuseReportMineRequest::default());
        re_encodes_identically(&AbuseReportWithdrawRequest {
            report_id: "0f".repeat(16),
            extra: BTreeMap::new(),
        });
        re_encodes_identically(&AbuseReportWithdrawReply::default());
        re_encodes_identically(&AbuseReportQueueRequest::default());
        re_encodes_identically(&AbuseReportResolveRequest {
            report_id: "0f".repeat(16),
            outcome: AbuseReportOutcome::Dismissed,
            extra: BTreeMap::new(),
        });
        re_encodes_identically(&AbuseReportResolveReply::default());
    }

    #[test]
    fn abuse_report_federation_triad_round_trips() {
        re_encodes_identically(&AbuseReportDeliverRequest {
            report_ref: "0f".repeat(16),
            subject: AbuseReportSubject::Post {
                cid: "ab".repeat(32),
            },
            reason: AbuseReportReason::Illegal,
            note: None,
            excerpt: None,
            origin_nest_id: "a.example".into(),
            origin_nest_url: Some("https://a.example".into()),
            subject_actor: Some("ef".repeat(32)),
            extra: BTreeMap::new(),
        });
        re_encodes_identically(&AbuseReportFederationWithdrawRequest {
            report_ref: "0f".repeat(16),
            extra: BTreeMap::new(),
        });
        re_encodes_identically(&AbuseReportFederationOutcomeRequest {
            report_ref: "0f".repeat(16),
            outcome: AbuseReportOutcome::Acted,
            extra: BTreeMap::new(),
        });
        re_encodes_identically(&AbuseReportFederationAck::default());
    }

    /// Additive evolution: a reason, status or outcome a newer peer adds
    /// decodes rather than failing the whole frame — a reason as `Other` (the
    /// report still lands), a status/outcome as `Unknown`.
    #[test]
    fn unknown_abuse_report_tokens_decode_to_their_catch_alls() {
        #[derive(Serialize)]
        struct Wire<'a> {
            reason: &'a str,
            status: &'a str,
            outcome: &'a str,
        }
        #[derive(Deserialize)]
        struct Read {
            reason: AbuseReportReason,
            status: AbuseReportStatus,
            outcome: AbuseReportOutcome,
        }
        let bytes = encode_canonical(&Wire {
            reason: "doxxing",
            status: "escalated",
            outcome: "referred",
        })
        .unwrap();
        let read: Read = decode(&bytes).unwrap();
        assert_eq!(read.reason, AbuseReportReason::Other);
        assert_eq!(read.status, AbuseReportStatus::Unknown);
        assert_eq!(read.outcome, AbuseReportOutcome::Unknown);
    }

    #[test]
    fn abuse_report_tokens_round_trip_through_storage() {
        for r in AbuseReportReason::ALL {
            assert_eq!(AbuseReportReason::from_token(r.token()), r);
        }
        assert_eq!(
            AbuseReportReason::from_token("nope"),
            AbuseReportReason::Other
        );
        for s in [
            AbuseReportStatus::Open,
            AbuseReportStatus::Resolved,
            AbuseReportStatus::Withdrawn,
        ] {
            assert_eq!(AbuseReportStatus::from_token(s.token()), s);
        }
        for o in [AbuseReportOutcome::Acted, AbuseReportOutcome::Dismissed] {
            assert_eq!(AbuseReportOutcome::from_token(o.token()), o);
        }
    }

    /// A newer build's subject vocabulary: one kind this build does not know.
    #[derive(Debug, Serialize, Deserialize)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    enum NewerPeerAbuseReportSubject {
        Post { cid: String },
        Group { group_id: String },
    }

    /// A subject kind a newer build added decodes into the carrying arm — its
    /// neighbours still decode — and re-encodes to the exact bytes it arrived
    /// as, so a reply that lists the report re-emits it unchanged. It is never
    /// read as an actor (rule 3).
    #[test]
    fn unknown_abuse_report_subject_is_carried_byte_for_byte() {
        let newer = vec![
            NewerPeerAbuseReportSubject::Post {
                cid: "ab".repeat(32),
            },
            NewerPeerAbuseReportSubject::Group {
                group_id: "cd".repeat(32),
            },
        ];
        let bytes = encode_canonical(&newer).unwrap();
        let decoded: Vec<AbuseReportSubject> = decode(&bytes).unwrap();
        assert_eq!(
            decoded[0],
            AbuseReportSubject::Post {
                cid: "ab".repeat(32)
            }
        );
        assert!(matches!(decoded[1], AbuseReportSubject::Unknown(_)));
        assert_eq!(decoded[1].kind(), "unknown");
        assert_eq!(encode_canonical(&decoded).unwrap(), bytes);
    }

    #[test]
    fn signal_contribute_round_trips() {
        re_encodes_identically(&ModerationSignalContributeRequest {
            content_id: "ab".repeat(32),
            signal: "watch-complete".into(),
            extra: BTreeMap::new(),
        });
        re_encodes_identically(&ModerationSignalContributeRequest::default());
        re_encodes_identically(&ModerationSignalContributeReply {
            status: "recorded".into(),
            signal: "skip".into(),
            extra: BTreeMap::new(),
        });
    }
}
