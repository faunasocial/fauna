//! Algorithm-filter, labeler, and scoring-bus types.
//!
//! Two live groups: the **filter** predicates (`FilterRule` / `FilterCombination`)
//! and the **labeler** registry + the `ScoreEntry` scoring-metadata bus that feed
//! composition consumes (`content-scoring.md` § The scoring-metadata bus).
//!
//! The module's original §12.10 "Distributed Scoring Protocol" — a WASM
//! `AlgorithmScorer`, firehose subscription negotiation, and score auditing —
//! was never wired to anything (its types carried `f64` fields the canonical
//! dag-cbor wire forbids, so they could not have ridden it) and was **deleted**
//! with the rest of the pre-frame surface (frame D9 —
//! `docs/goal/behavior/engagement-cues.md` § Retirement). Nest-side content
//! scoring is not a thing Fauna does; scoring rides the factor bus.

use ed25519_dalek::{Signature, Signer, SigningKey};
use serde::{Deserialize, Serialize};

use crate::data::{ContentHash, MutedKeyword, Timestamp};
use crate::identity::ActorId;
use crate::mail_auth::{ArcVerdict, AuthVerdicts, DkimVerdict, DmarcVerdict, SpfVerdict};
use crate::mail_scan::{ClamavVerdict, RspamdScore};

/// How a feed's rules combine. Never serialized: the wire and the stored feed
/// carry the string `"all"` / `"any"`, which each reader projects.
#[derive(Debug, Clone, Copy)]
pub enum FilterCombination {
    All,
    Any,
}

/// One feed-rule predicate.
///
/// **Open, carrying** (`transport.md` § Rule 3 in full): the nest stores a
/// feed's rules as canonical dag-cbor and an app echoes them whole on
/// `fauna.feed.update`, so a rule a newer writer added must survive an older
/// reader byte for byte — [`FilterRule::Unknown`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FilterRule {
    MinReplies {
        count: u32,
    },
    MinReposts {
        count: u32,
    },
    HasMedia {
        required: bool,
    },
    BodyHint {
        hints: Vec<crate::data::BodyHint>,
    },
    AuthorInSet {
        #[serde(with = "serde_bytes")]
        actors: [u8; 32],
    },
    AuthorNotInSet {
        #[serde(with = "serde_bytes")]
        actors: [u8; 32],
    },
    IsReply {
        required: bool,
    },
    HasHashtag {
        tags: Vec<String>,
    },
    CreatedAfter {
        age_microseconds: u64,
    },
    BodyContains {
        terms: Vec<String>,
    },
    BodyExcludes {
        terms: Vec<String>,
    },
    Source {
        protocols: Vec<String>,
    },
    LabelBelow {
        category: String,
        /// confidence × 1000 (per-mille); the dag-cbor wire forbids floats — serialization.md:54
        max_confidence_permille: u16,
    },
    LabelAbove {
        category: String,
        /// confidence × 1000 (per-mille); the dag-cbor wire forbids floats — serialization.md:54
        min_confidence_permille: u16,
    },
    HasLabel {
        category: String,
    },
    /// A rule a newer writer added that this build cannot read, held as the
    /// whole undecoded value and re-encoded unchanged (`encode_canonical`
    /// re-emits canonical input byte for byte). It **never matches**: a feed
    /// holding one yields nothing from discovery, its rule set evaluates the
    /// unknown condition as false, and no form offers or re-authors it — so it
    /// never shows what a known rule would have hidden.
    #[serde(untagged)]
    Unknown(fauna_cbor::Value),
}

impl FilterRule {
    /// Whether this build can evaluate the rule — false only for the open arm.
    pub fn is_known(&self) -> bool {
        !matches!(self, FilterRule::Unknown(_))
    }
}

// --- Content Intelligence (Layer 2) ---

/// Sandbox resource ceilings for a published WASM module.
///
/// Named for the `AlgorithmScorer` that once shared it; that pre-frame scorer /
/// firehose / audit surface was never wired to anything and is now retired (frame
/// D9 — `docs/goal/behavior/engagement-cues.md` § Retirement), so today the sole
/// user is [`AlgorithmLabeler`]. Kept under the old name on purpose: it crosses the
/// `fauna-ffi` UniFFI boundary, so renaming it churns the generated Go/Kotlin/Swift
/// bindings — a cosmetic rename not worth bundling into a deletion.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ScorerLimits {
    pub max_memory_bytes: u64,
    pub max_cpu_microseconds: u64,
}

/// Published WASM labeler metadata — the registry entry behind the live
/// labeler runtime (`fauna-labeler`, `labeler_handlers`, `db/labelers.rs`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlgorithmLabeler {
    pub algorithm_id: ActorId,
    pub version: u64,
    pub wasm_hash: ContentHash,
    pub wasm_size: u64,
    pub input_schema: LabelerInput,
    /// The revision of the [`Label`] record the module emits — the output half
    /// of the `label()` ABI, signed beside the input declaration. Absent reads
    /// revision 1 and revision 1 is omitted, so every artifact published
    /// before the field stays byte-identical and verifies.
    #[serde(default, skip_serializing_if = "LabelerOutput::is_v1")]
    pub output_schema: LabelerOutput,
    pub resource_limits: ScorerLimits,
    pub updated_at: Timestamp,
    #[serde(with = "serde_bytes")]
    pub signature: Vec<u8>,
}

/// The `Label` record revision this build reads from a module's output.
///
/// Raised by a new [`LabelSource`] variant or a new [`Label`] field — together
/// with the `LabelSource` line of
/// `tools/check-additive-evolution/enum_ledger.txt`, in the same commit — and
/// every earlier revision stays decodable for the rest of the major
/// (`content-moderation-and-ranking.md` § Tier-3 → *The output half of the
/// `label()` ABI*).
pub const LABEL_ABI_CURRENT: u16 = 1;

/// Declares what a WASM labeler module emits: the output half of the `label()`
/// ABI (`content-moderation-and-ranking.md` § Tier-3 → *The output half of the
/// `label()` ABI*).
///
/// The output is a positional BARE `Vec<Label>`, so nothing about it is
/// additive: the module's signed `label_abi` is the stamp a runner reads
/// **before** the BARE decode, refusing a revision newer than
/// [`LABEL_ABI_CURRENT`] as newer rather than mis-reading it as broken. Like
/// [`LabelerInput::needs_attachment_bytes`] the field is additive on the
/// signed record: revision 1 is omitted, so a holder that predates it verifies
/// every revision-1 artifact and refuses a later one as unverified. Pinned by
/// `labeler_tests::{a_revision_one_output_schema_encodes_byte_identically_to_the_pre_stamp_shape,
/// a_pre_stamp_holder_refuses_a_newer_revision_artifact_as_unverified}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabelerOutput {
    pub label_abi: u16,
}

impl Default for LabelerOutput {
    fn default() -> Self {
        Self { label_abi: 1 }
    }
}

impl LabelerOutput {
    /// Whether this declares revision 1 — the shape every module before the
    /// stamp emitted, and the one the wire omits.
    pub fn is_v1(&self) -> bool {
        self.label_abi == 1
    }
}

/// Declares what input fields a WASM labeler module requires.
///
/// Every flag is a **declaration the subscriber inspects before subscribing**
/// (`content-moderation-and-ranking.md` § Tier-3: the artifact is transparent),
/// signed with the rest of the metadata. The first four are pure declaration —
/// the host hands a module every v1 field regardless. The fifth,
/// [`needs_attachment_bytes`](Self::needs_attachment_bytes), also **selects
/// the input shape** the host encodes (§ Tier-3 → *The attachment facet*):
/// unset, the module reads [`LabelerPostInput`] exactly as every module before
/// the flag did; set, it reads [`LabelerInputWithAttachments`].
///
/// The fifth flag is additive on a signed record (`verify_labeler_metadata`
/// signs the canonical re-encode): `skip_serializing_if` keeps every artifact
/// that predates it **byte-identical**, so its stored signature still verifies
/// on a holder that knows the flag, and `default` reads that artifact as not
/// asking. A holder that predates the flag decodes a flagged artifact without
/// it, re-encodes without it, and so **refuses it as unverified** — at publish,
/// or at inspect (`LabelerInspectView::verified` reads false and subscribe
/// stays disabled). That is the intended posture: a holder that cannot supply
/// what a module declares it reads must never run it without, and one that
/// cannot show a subscriber what the module reads must not call it verified.
/// Pinned by `labeler_tests::{a_flagless_input_schema_encodes_byte_identically_to_the_pre_flag_shape,
/// an_older_holder_refuses_a_bytes_asking_artifact_rather_than_running_it_without}`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct LabelerInput {
    pub needs_text: bool,
    pub needs_hashtags: bool,
    pub needs_media_metadata: bool,
    pub needs_author: bool,
    /// The module reads the item's attachment **bytes** (the facet of
    /// [`LabelerInputWithAttachments`]), not only their declared metadata. Only
    /// a position that holds the bytes fills the facet — a community room's
    /// home nest, inside the read its members minted it
    /// (`conversation-rooms.md` § The three classes → *What the read covers*);
    /// every other position hands the declared shape with an empty facet.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub needs_attachment_bytes: bool,
}

/// The input passed to a WASM labeler's `label()` export function.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabelerPostInput {
    pub text: Option<String>,
    pub hashtags: Vec<String>,
    pub has_media: bool,
    pub media_type: Option<String>,
    pub duration_ms: Option<u64>,
    pub author: ActorId,
}

/// One attachment of a labelled item, as a module that declared
/// [`LabelerInput::needs_attachment_bytes`] reads it — the attachment facet's
/// element (`content-moderation-and-ranking.md` § Tier-3 → *The attachment
/// facet*).
///
/// `bytes` is the attachment's **opened plaintext**, or **empty when the host
/// withheld it**: the attachment is over the per-attachment ceiling, the facet
/// is already at its total ceiling, the blob did not open, or the position
/// holds no bytes at all (the mail holder, a post position). `size_bytes` is
/// the author's declared plaintext size either way, so a module can tell a
/// withheld attachment from an empty one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabelerAttachmentInput {
    pub mime_type: String,
    pub size_bytes: u64,
    #[serde(with = "serde_bytes")]
    pub bytes: Vec<u8>,
}

/// The `label()` input for a module that declared
/// [`LabelerInput::needs_attachment_bytes`]: the post-shaped input every
/// module reads, followed by the attachment facet — the item's attachments in
/// the author's order.
///
/// BARE encodes a struct as its fields in order with no framing, so these
/// bytes are exactly [`LabelerPostInput`]'s followed by the facet's — a module
/// author decodes the v1 shape and then the facet, and a holder that already
/// holds the v1 bytes appends the facet
/// (`fauna_labeler::run_published_labeler_bare`). Pinned by
/// `fauna-labeler`'s `a_flagged_input_is_the_post_input_followed_by_the_facet`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabelerInputWithAttachments {
    pub post: LabelerPostInput,
    pub attachments: Vec<LabelerAttachmentInput>,
}

impl LabelerPostInput {
    /// The `label()` input for one post, read off what the post itself says —
    /// its body text, its tag names, its first attachment's type, a video's
    /// length, its author.
    ///
    /// The one home for this mapping, so every position that runs a module over
    /// a post (the nest's public web render running a region document's bundled
    /// scorers today; an app running the same scorers on-device) hands the
    /// module the same fields. For a gated post `post.body` is the public
    /// preview, and that is all this reads: the sealed full body is not public
    /// plaintext and never becomes a module's input here.
    pub fn from_post(post: &crate::data::Post) -> Self {
        let text = post.body_text();
        let (media_type, duration_ms) = match &post.body {
            crate::data::PostBody::Video { duration_ms, .. } => (None, Some(*duration_ms)),
            body => (
                body.media_items().first().map(|m| m.media_type.clone()),
                None,
            ),
        };
        Self {
            text: (!text.is_empty()).then_some(text),
            hashtags: post.tags(),
            has_media: post.body.has_media(),
            media_type,
            duration_ms,
            author: post.author,
        }
    }
}

/// A single label with category and confidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Label {
    pub category: String,
    pub confidence: f64,
    pub source: LabelSource,
}

/// How the label was derived.
///
/// **Closed by design** (`transport.md` § Schema and forward-compat discipline
/// → *Rule 3 in full*, the `ladder` ground: a new variant raises the module's
/// signed label-ABI stamp, [`LabelerOutput::label_abi`], which the runner
/// reads before the BARE decode, so there is no unknown arm). A new variant
/// raises [`LABEL_ABI_CURRENT`] and edits
/// `tools/check-additive-evolution/enum_ledger.txt`, in the same change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LabelSource {
    Manual,
    TextAnalysis,
    VisionModel,
    AudioModel,
    Composite,
}

// ── The scoring-metadata bus (content-scoring.md § The scoring-metadata bus) ─
//
// One row per scored factor on the uniform per-item `scores` array — the
// multi-factor generalization of the mail-specific score fields
// (`content-moderation-and-ranking.md` § Resolved design decisions Q1). Rides
// the bridge ingest wire (`fauna-protocol::bridge_routing::
// IngestInboundMailRequest.scores`), the segment floor
// (`fauna-mail::segments::floor`, a field-for-field mirror — that crate's
// codec feature deliberately pulls only `fauna-cbor`), and the nest's
// `content_scores` table. Writing a factor's row is the `content.label-write`
// operation at the key-access layer (a label is floor/deployment-data and
// carries no content key — capability-mediated content-processing design
// § capability-scope taxonomy); enforcement of that scope ships with the
// capability substrate, not this type.

/// One factor's row on the scoring-metadata bus.
///
/// Metadata only — a score/label/verdict, never the content that produced it
/// (`content-scoring.md` § bus rule). No `deny_unknown_fields`: entries are
/// embedded in at-rest floors, and an older binary must keep decoding a floor
/// whose entries grew an additive field (version-compatibility.md I2).
///
/// A `uniffi::Record` so the Go MTA receives the rows
/// [`perimeter_mail_score_rows`] mints at the ingest edge (the contract phase
/// of the bus: the perimeter emits its own rows, the nest derives nothing).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ScoreEntry {
    /// Factor name — one of [`factor`]'s constants for the built-in mail
    /// factors; community/admin labelers add theirs.
    pub factor: String,
    /// Integer per-mille (milli-int, signed — ham/negative contributions
    /// allowed). The dag-cbor wire forbids floats.
    pub score: i64,
    /// Model-authority tier: [`TIER_USER`] / [`TIER_ADMIN`] /
    /// [`TIER_COMMUNITY`] (`content-moderation-and-ranking.md` § The three
    /// model-authority tiers).
    pub tier: u8,
    /// The scorer's version watermark. Drives the re-score obligation: a
    /// capability-holder drain compares this against the current
    /// model-version registry and re-scores on a gap (frame § Tier-3;
    /// consumer: the content-at-rest drain worker). Type matches
    /// `IndexManifest.tokenizer_version` (`u32`), the sibling watermark.
    pub scorer_version: u32,
}

/// Tier 1 — the user's own voluntary model (e.g. the per-user Bayesian spam
/// factor); sealed/private at rest.
pub const TIER_USER: u8 = 1;
/// Tier 2 — deployment/admin/authority, compulsory, transparent (e.g. the
/// perimeter ClamAV / rspamd / auth-verdict scorers).
pub const TIER_ADMIN: u8 = 2;
/// Tier 3 — community models, opt-in, transparent.
pub const TIER_COMMUNITY: u8 = 3;

/// Factor names for the built-in mail factors (the bus proto-instance).
pub mod factor {
    /// Per-user spam score (per-mille; `mail-spam.md`). Tier 1.
    pub const SPAM: &str = "spam";
    /// Perimeter ClamAV verdict as per-mille (0 clean / 1000 infected). Tier 2.
    pub const CLAMAV: &str = "clamav";
    /// Perimeter rspamd scaled score (milli-int; `mail-content-scanning.md`).
    /// Tier 2.
    pub const RSPAMD: &str = "rspamd";
    /// SPF verdict as per-mille (0 pass / 1000 fail-family; indeterminate
    /// verdicts emit no row). Tier 2.
    pub const AUTH_SPF: &str = "auth_spf";
    /// DKIM verdict as per-mille. Tier 2.
    pub const AUTH_DKIM: &str = "auth_dkim";
    /// DMARC verdict as per-mille. Tier 2.
    pub const AUTH_DMARC: &str = "auth_dmarc";
    /// ARC verdict as per-mille. Tier 2.
    pub const AUTH_ARC: &str = "auth_arc";
    /// k-anonymized distributed spam-report aggregate as per-mille
    /// (`report-sharing.md` § The aggregate). Tier 3. Deliberately NOT in
    /// [`super::builtin_factor_versions`]: the nest itself is this factor's
    /// scorer (it rewrites the rows on every aggregate transition), so a
    /// model-version-registry entry would create re-score obligations no
    /// capability-holder can serve.
    pub const REPORT_SPAM: &str = "report:spam";
    /// k-anonymized distributed **engagement-cue** aggregates
    /// (`engagement-cues.md` § Layer B — the `signal:*` factors). Tier 3. The
    /// opt-in Layer-B siblings of [`REPORT_SPAM`]: a coarse per-item verdict
    /// ("I watched this to the end" / "I skipped this") that, at ≥ k distinct
    /// opted-in local contributors, becomes a transparent aggregate on the
    /// SAME `content_reports` table, through the SAME k-gate + count curve
    /// ([`super::reports`]) and the SAME `fauna.federation.reports.{exchange,
    /// export}` pair — no new tables, no new federation kinds. Public posts
    /// only (a verdict about restricted content is rejected at the write path —
    /// its existence would leak readership). A user has ONE verdict per item,
    /// so the two factors are mutually exclusive per `(reporter, content_hash)`
    /// — a flip withdraws one and inserts the other. Like [`REPORT_SPAM`],
    /// deliberately NOT in [`super::builtin_factor_versions`] (the nest is the
    /// scorer — it rewrites the rows on every aggregate transition).
    pub const SIGNAL_WATCH_COMPLETE: &str = "signal:watch-complete";
    /// The `skip` sibling of [`SIGNAL_WATCH_COMPLETE`] (an item shown but
    /// skipped past). Same table / gate / curve / exchange; mutually exclusive
    /// with `watch-complete` per contributor.
    pub const SIGNAL_SKIP: &str = "signal:skip";
    /// The nest-computed trend-velocity factor (`trending.md` § The factor):
    /// metadata-only (explicit-act event log + timestamps, never content),
    /// public posts only, `actor_id = NULL` rows. Like [`REPORT_SPAM`],
    /// deliberately NOT in [`super::builtin_factor_versions`]: the nest is
    /// the scorer (transition + sweep recompute), so a registry entry would
    /// create re-score obligations no capability-holder can serve.
    pub const TRENDING: &str = "trending";
    /// The nest-computed cumulative-engagement scalar
    /// ([`super::engagement::engagement_score`], persisted on
    /// `content_meta.score`) serving as one composable factor among many (frame
    /// § Composition — "the single nest-computed recency/engagement score becomes
    /// one factor among many"). NOT a `content_scores` row: the composed feed
    /// query reads it straight from `content_meta` as an f64 in `[0, 1)` and
    /// multiplies by the caller's weight (`db/feeds.rs`), mixing it with bus
    /// factors on one scale. The **decay-free sibling of [`TRENDING`]** — all-time
    /// accumulated interaction weight, where trending is decayed velocity (module
    /// [`super::engagement`]). Like [`REPORT_SPAM`], deliberately NOT in
    /// [`super::builtin_factor_versions`] (no bus rows → no re-score obligation to
    /// serve).
    pub const ENGAGEMENT: &str = "engagement";
    /// The deterministic muted-keywords penalty scorer (frame § Composition /
    /// Q3): a **sealed tier-1** factor over the user's
    /// `fauna.state.moderation` muted-keywords list, which the nest can never
    /// read — so it has NO nest-side `content_scores` rows and composes
    /// **client-side** post-decrypt ([`super::muted_keywords_penalty_entry`]).
    /// Nest-side its term in a composition is always 0 (the sealed-factor
    /// seam). Like [`ENGAGEMENT`], deliberately NOT in
    /// [`super::builtin_factor_versions`].
    pub const MUTED_KEYWORDS: &str = "muted-keywords";
}

/// Version watermarks for the built-in factor→bus mappings. Bump a factor's
/// constant when its scorer (or [`perimeter_mail_score_rows`]' mapping of its
/// verdict onto a row) changes meaning, so the re-score obligation fires for
/// rows written under the old version.
pub mod scorer_version {
    pub const SPAM: u32 = 1;
    /// The deterministic muted-keywords matcher version
    /// (`keyword::body_excludes_matches` semantics — case-insensitive
    /// substring, OR across terms). Client-side only; bump if the matcher's
    /// semantics ever change so clients can re-derive collapsed/sunk state.
    pub const MUTED_KEYWORDS: u32 = 1;
    pub const CLAMAV: u32 = 1;
    pub const RSPAMD: u32 = 1;
    pub const AUTH: u32 = 1;
    /// The report-sharing score-from-count curve version (`report-sharing.md`
    /// § The aggregate). On a bump the nest recomputes its own rows (the
    /// aggregate writer is nest-side, not the holder drain — see
    /// [`factor::REPORT_SPAM`]). **Also stamps the `signal:*` cue aggregates**
    /// ([`factor::SIGNAL_WATCH_COMPLETE`]/[`factor::SIGNAL_SKIP`]): they ride
    /// the identical [`super::reports`] curve, so their version IS the report
    /// curve version (the shared-scorer precedent of [`AUTH`]). Bump this and
    /// both families recompute together.
    pub const REPORT: u32 = 1;
    /// The trend-velocity curve version (`trending.md` § The factor). Bumps
    /// on any curve change — decay, saturation, weights, or the peer ramp —
    /// and recomputes nest-side exactly like [`REPORT`] (see
    /// [`factor::TRENDING`]).
    pub const TREND: u32 = 1;
}

// ── Feed composition (frame § Composition — feeds are composable scorers) ──

/// Maximum entries a feed composition may carry. Bounds the per-query SQL
/// work the nest does per composed feed (one correlated bus lookup per
/// entry); refutable constant, sized far above any real authoring UI.
pub const MAX_COMPOSITION_ENTRIES: usize = 64;

/// One factor's term in a feed composition
/// (`content-moderation-and-ranking.md` § Composition, ratified 2026-07-06):
/// the composed ordering key is
/// `final = Σ (weight_permille · factor_value) / 1000`, and the feed orders
/// by it. A **sort** factor carries a positive weight; a **filter** is a
/// factor whose contribution is strongly negative (a muted-keyword scorer's
/// −1000 score at weight 1000 sinks an item below any rendered page — soft,
/// reversible, no separate filter code path).
///
/// `factor` is an opaque bus key (`content_scores.factor`): bare built-ins
/// (the [`factor`] constants), namespaced community factors
/// (`labeler:<hex>`, `report:spam`, …), or [`factor::ENGAGEMENT`] — the
/// nest-computed recency/engagement score serving as one factor among many.
/// Never an enum: the factor namespace is open-ended by design.
///
/// Scope is by container, not a flag: an entry on a feed's definition
/// applies to that feed; an entry in the user's global factor set folds into
/// every one of their feeds. The same factor in both simply sums —
/// conflicts resolve by arithmetic (frame § Composition).
///
/// At rest a composition is the canonical dag-cbor of
/// `Vec<CompositionEntry>` (`feeds.composition`, same codec discipline as
/// `feeds.rules`); integer weights because the dag-cbor wire forbids floats,
/// mirroring [`ScoreEntry::score`]'s per-mille convention. No
/// `deny_unknown_fields`: an older binary must keep decoding a composition
/// whose entries grew an additive field (version-compatibility.md I2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompositionEntry {
    /// Opaque factor key on the scoring-metadata bus.
    pub factor: String,
    /// Signed per-mille weight (1000 = 1.0×).
    pub weight_permille: i64,
}

/// Why a composition failed [`validate_composition`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CompositionError {
    /// More than [`MAX_COMPOSITION_ENTRIES`] entries.
    #[error("composition has {0} entries (max {MAX_COMPOSITION_ENTRIES})")]
    TooManyEntries(usize),
    /// The entry at this index has an empty `factor` key.
    #[error("composition entry {0} has an empty factor")]
    EmptyFactor(usize),
    /// The same factor appears twice (ambiguous — the authoring UI edits one
    /// weight per factor; arithmetic merging across *containers* is the
    /// feed-vs-global rule, never within one list).
    #[error("composition repeats factor {0:?}")]
    DuplicateFactor(String),
}

/// The muted-keywords penalty (per-mille): the frame's canonical "strong
/// negative" — at weight 1000 it contributes −1000, swamping any promotion
/// and sinking the item below any rendered page (soft, reversible). It is also
/// the strongest weight a muted keyword can carry, and the one weight that
/// collapses ([`muted_keywords_collapse`]).
pub const MUTED_KEYWORDS_PENALTY: i64 = -1000;

/// The weight a keyword is muted at until the user says otherwise — the full
/// penalty (`content-moderation-and-ranking.md` § Composition, the 2026-07-10
/// per-keyword weight ruling).
pub const MUTED_KEYWORD_DEFAULT_WEIGHT: i64 = MUTED_KEYWORDS_PENALTY;

/// The weight the muted-words page's **Show less** level mutes at — the
/// midpoint of the range. Far enough from `0` to be a demotion a user notices
/// in a ranked feed, far enough from the full penalty that a hard mute still
/// wins the strongest-weight rule over it, and one constant so every app
/// demotes identically (a value no human chooses — it is not a setting).
pub const MUTED_KEYWORD_SHOW_LESS_WEIGHT: i64 = -500;

/// How hard a muted keyword mutes, as the muted-words page offers it
/// (`docs/goal/ui/settings.md` § Muted words — the level picker): two levels
/// over the per-mille weight the record keeps
/// (`content-moderation-and-ranking.md` § Composition, the 2026-07-10 ruling
/// and its 2026-10-02 presentation). The record stays the full `[−1000, 0]`
/// range, so a finer control later is an additive change, never a migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "kebab-case")]
pub enum MutedKeywordLevel {
    /// The full penalty ([`MUTED_KEYWORDS_PENALTY`]) — the default a new term
    /// mutes at: a match collapses the post or message behind its reveal and
    /// sinks the post in every ranked feed.
    Hide,
    /// [`MUTED_KEYWORD_SHOW_LESS_WEIGHT`] — a match sinks the post in a ranked
    /// feed and nothing else: no collapse anywhere, and a conversation, which
    /// is not ranked, shows the message as usual.
    ShowLess,
}

impl MutedKeywordLevel {
    /// The weight this level stores.
    pub const fn weight(self) -> i64 {
        match self {
            Self::Hide => MUTED_KEYWORDS_PENALTY,
            Self::ShowLess => MUTED_KEYWORD_SHOW_LESS_WEIGHT,
        }
    }

    /// The level a stored weight reads back as — by the collapse threshold,
    /// the one decision [`muted_keywords_collapse`] makes: `Hide` iff the
    /// weight collapses, else `ShowLess`. So a weight set by any other means
    /// never renders as a hide it does not perform, and the round trip
    /// `of(level.weight()) == level` holds for both levels.
    pub fn of(weight: i64) -> Self {
        if clamp_muted_keyword_weight(weight) <= MUTED_KEYWORDS_PENALTY {
            Self::Hide
        } else {
            Self::ShowLess
        }
    }
}

/// A muted keyword's weight, read into its range `[MUTED_KEYWORDS_PENALTY, 0]`:
/// the penalty factor only ever demotes, and never past the full penalty. Every
/// reader clamps, so a stored value out of range degrades to its nearest bound
/// rather than turning the mute into a promotion.
pub fn clamp_muted_keyword_weight(weight: i64) -> i64 {
    weight.clamp(MUTED_KEYWORDS_PENALTY, 0)
}

/// The deterministic tier-1 keyword-penalty scorer (frame § Composition —
/// "muted keywords are a deterministic tier-1 penalty scorer, composed
/// globally"): the canonical matcher [`crate::keyword::body_excludes_matches`]
/// over each entry of the user's sealed `fauna.state.moderation`
/// muted-keywords list. A match yields a [`ScoreEntry`] ([`factor::MUTED_KEYWORDS`],
/// [`TIER_USER`]) whose score is the matching keyword's weight; when several
/// keywords match, the **strongest** (most negative) weight, never their sum —
/// the factor stays one value in `[MUTED_KEYWORDS_PENALTY, 0]`, so naming three
/// softly muted terms cannot sink a post past a hard mute. No match yields
/// nothing.
///
/// Because the list is sealed under the user's key, this factor composes
/// **client-side** (or at a capability-holder) — the nest never sees the
/// list, the body match, or the resulting entry, and its nest-side
/// composition term is always 0. Do not duplicate the matcher or the list;
/// this wrapper is the single scorer definition every app composes with.
pub fn muted_keywords_penalty_entry(
    muted_keywords: &[MutedKeyword],
    body: &str,
) -> Option<ScoreEntry> {
    muted_keywords
        .iter()
        .filter(|k| crate::keyword::body_excludes_matches(std::slice::from_ref(&k.keyword), body))
        .map(|k| clamp_muted_keyword_weight(k.weight))
        .min()
        .map(|score| ScoreEntry {
            factor: factor::MUTED_KEYWORDS.to_string(),
            score,
            tier: TIER_USER,
            scorer_version: scorer_version::MUTED_KEYWORDS,
        })
}

/// Does `body` collapse behind the muted-content reveal? Only when a keyword
/// muted at the full penalty matches: the collapse is the hide verb, and a
/// softer weight asks for less of a term, not for it hidden. A soft match still
/// demotes a post in a ranked feed ([`muted_keywords_penalty_entry`]); in a
/// conversation, which is not ranked, it changes nothing.
pub fn muted_keywords_collapse(muted_keywords: &[MutedKeyword], body: &str) -> bool {
    muted_keywords_penalty_entry(muted_keywords, body)
        .is_some_and(|entry| entry.score <= MUTED_KEYWORDS_PENALTY)
}

/// Validate a feed composition's contract: ≤ [`MAX_COMPOSITION_ENTRIES`]
/// entries, non-empty factor keys, no duplicate factors. Weights are
/// unrestricted `i64` (extreme negatives are the filter verb). Shared by the
/// nest create/update gate and any client that pre-validates an authoring
/// form.
pub fn validate_composition(entries: &[CompositionEntry]) -> Result<(), CompositionError> {
    if entries.len() > MAX_COMPOSITION_ENTRIES {
        return Err(CompositionError::TooManyEntries(entries.len()));
    }
    let mut seen = std::collections::BTreeSet::new();
    for (index, entry) in entries.iter().enumerate() {
        if entry.factor.is_empty() {
            return Err(CompositionError::EmptyFactor(index));
        }
        if !seen.insert(entry.factor.as_str()) {
            return Err(CompositionError::DuplicateFactor(entry.factor.clone()));
        }
    }
    Ok(())
}

/// Distributed report sharing — the k-anonymity gate + score-from-count curve
/// (`report-sharing.md` § The k-anonymity choke point + § The aggregate).
/// Shared Rust so clients can render exactly the numbers a nest computes.
pub mod reports {
    /// The k-anonymity floor: a per-item report aggregate becomes readable
    /// *anywhere* (bus, federation export, transparency surface — admin
    /// included) only at ≥ this many distinct local reporters. A hard-coded
    /// safety constant, deliberately NOT a knob on any surface — a privacy
    /// floor, not a preference (the `BASELINE_MIN_CONTRIBUTORS` decision,
    /// `mail-spam.md` § Cold start Path 2, transfers verbatim).
    pub const REPORT_MIN_REPORTERS: u32 = 3;

    /// The per-reporter cap on user-initiated abuse reports
    /// (`moderation.md` § User-initiated reporting → *Anti-abuse posture*
    /// bound 2): a reporter past this many reports in a rolling hour is
    /// refused `rate_limited`. A hard-coded constant, no knob on any surface —
    /// generous enough that nobody reporting honestly meets it, tight enough
    /// that a flood costs an admin minutes, not days. Unrelated to the
    /// k-anonymized aggregates above (a report never feeds them).
    pub const ABUSE_REPORTS_PER_HOUR: u32 = 20;

    /// Flat per-mille contribution of the single non-scaling peer
    /// corroboration bucket (`report-sharing.md` § The aggregate;
    /// `federation.md` § Reputation exchange — peer `nest_id`s are self-minted,
    /// so claimed counts and peer multiplicity must buy nothing).
    pub const PEER_CORROBORATION_PM: i64 = 100;

    /// The factors `fauna.federation.reports.{exchange,export}` carry
    /// (`report-sharing.md` § Federation exchange): the spam-report aggregate
    /// and the Layer-B engagement-cue aggregates — every factor the nest
    /// itself writes through the `content_reports` k-gate, and nothing else.
    pub const EXCHANGED_FACTORS: [&str; 3] = [
        super::factor::REPORT_SPAM,
        super::factor::SIGNAL_WATCH_COMPLETE,
        super::factor::SIGNAL_SKIP,
    ];

    /// Whether a peer's report entry may write `factor`. The exchange is
    /// open-federation with no `content.label-write` capability, so an entry
    /// naming any other bus factor (`clamav`, `labeler:*`, `trending`, …) is
    /// skipped on import — otherwise the aggregate writer's replace-by-key
    /// would overwrite that factor's local row.
    pub fn is_exchanged_factor(factor: &str) -> bool {
        EXCHANGED_FACTORS.contains(&factor)
    }

    /// THE k-anonymity choke point (`report-sharing.md` § The k-anonymity
    /// choke point): every surface that exposes a per-item report aggregate —
    /// the bus-row writer, the federation export, the transparency read —
    /// consumes this gate, never the raw table. `None` below the floor.
    pub fn exposed_report_count(local_count: u32) -> Option<u32> {
        (local_count >= REPORT_MIN_REPORTERS).then_some(local_count)
    }

    /// Score-from-count curve, integer per-mille (`report-sharing.md` § The
    /// aggregate): 0 below the k floor; a linear ramp from 200‰ at k that
    /// saturates at 1000‰ by ~50 local reporters; plus the flat peer bucket.
    /// Peer-only (below-k local + a peer bucket) is exactly
    /// [`PEER_CORROBORATION_PM`] — visible corroboration, never consensus.
    pub fn report_score_pm(local_count: u32, peer_bucket_present: bool) -> i64 {
        let local = match exposed_report_count(local_count) {
            Some(n) => (200 + i64::from(n - REPORT_MIN_REPORTERS) * 17).min(1000),
            None => 0,
        };
        let peer = if peer_bucket_present {
            PEER_CORROBORATION_PM
        } else {
            0
        };
        (local + peer).min(1000)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn gate_opens_exactly_at_k() {
            assert_eq!(exposed_report_count(0), None);
            assert_eq!(exposed_report_count(2), None);
            assert_eq!(exposed_report_count(3), Some(3));
            assert_eq!(exposed_report_count(100), Some(100));
        }

        #[test]
        fn curve_shape() {
            // Below k: nothing, regardless of the peer bucket being absent.
            assert_eq!(report_score_pm(0, false), 0);
            assert_eq!(report_score_pm(2, false), 0);
            // At k: modest.
            assert_eq!(report_score_pm(3, false), 200);
            // Monotone ramp…
            assert_eq!(report_score_pm(10, false), 200 + 7 * 17);
            // …saturating by ~50 and capped at 1000.
            assert_eq!(report_score_pm(50, false), 999);
            assert_eq!(report_score_pm(51, false), 1000);
            assert_eq!(report_score_pm(10_000, false), 1000);
        }

        #[test]
        fn only_report_and_signal_factors_are_exchanged() {
            use super::super::factor;
            for f in [
                factor::REPORT_SPAM,
                factor::SIGNAL_WATCH_COMPLETE,
                factor::SIGNAL_SKIP,
            ] {
                assert!(is_exchanged_factor(f), "{f}");
            }
            for f in [
                factor::CLAMAV,
                factor::RSPAMD,
                factor::SPAM,
                factor::AUTH_DKIM,
                factor::TRENDING,
                "labeler:x",
                "signal:other",
                "",
            ] {
                assert!(!is_exchanged_factor(f), "{f}");
            }
        }

        #[test]
        fn peer_bucket_is_flat_and_never_consensus() {
            // Peer-only: exactly the flat corroboration, no local consensus.
            assert_eq!(report_score_pm(0, true), PEER_CORROBORATION_PM);
            assert_eq!(report_score_pm(2, true), PEER_CORROBORATION_PM);
            // Corroborates a local consensus additively, capped.
            assert_eq!(report_score_pm(3, true), 300);
            assert_eq!(report_score_pm(51, true), 1000);
        }
    }
}

/// Trend-velocity curves, constants, and the export k-gate (`trending.md` —
/// the `reports` module's sibling). All constants are hard-coded safety /
/// mechanism values, deliberately not knobs on any surface; shared Rust so
/// clients can render the same numbers a nest computes. Curve math is
/// internal `f64` (the float ban is a *wire* rule); every output is integer
/// per-mille.
pub mod trends {
    /// Velocity decay half-life (`trending.md` § Local velocity): an
    /// explicit-act event contributes half its weight after this many hours.
    pub const TREND_HALF_LIFE_HOURS: f64 = 6.0;

    /// Velocity saturation constant: `v = TREND_SATURATION_V` maps to 500‰
    /// (half of the local ceiling).
    pub const TREND_SATURATION_V: f64 = 20.0;

    /// The bounded distinct-peer ramp ceiling (frame carve-out D7): with zero
    /// local engagement, full peer corroboration tops out here — far below
    /// saturation, so any locally-engaged post outranks a peer-only one.
    pub const TREND_PEER_CAP_PM: i64 = 300;

    /// The k-anonymity floor on the **cross-boundary** disclosure: a local
    /// trend entry crosses a wire (export) only when backed by at least this
    /// many distinct local engagers. Local rows are deliberately NOT k-gated
    /// (`trending.md` § The k-anonymity gate — the per-post counters are
    /// already public on this nest; the gate protects the new "this nest's
    /// users engaged" information).
    pub const TREND_MIN_ENGAGERS: u32 = 3;

    /// Per-event-kind velocity weight in **milli** units (`trending.md`
    /// § Local velocity: like 1.0, reply 1.5, repost 2.0, quote 2.0).
    /// `None` for anything else — views are deliberately excluded in v1, and
    /// an unknown kind contributes nothing (fail-quiet on vocabulary growth).
    /// The kind strings are the `engagement_events.event_type` vocabulary the
    /// explicit-act writers use (`fauna.posts.interact` / the post-create
    /// reference counters).
    pub fn event_weight_milli(event_type: &str) -> Option<u32> {
        match event_type {
            "like" => Some(1000),
            "reply" => Some(1500),
            "repost" | "quote" => Some(2000),
            _ => None,
        }
    }

    /// The decayed local velocity: `Σ w(kind) · 2^(−age / half-life)` over
    /// qualifying events, ages in **seconds** (callers convert from their
    /// storage unit; negative ages — clock skew — clamp to 0, never amplify).
    pub fn local_velocity<'a>(events: impl Iterator<Item = (&'a str, i64)>) -> f64 {
        events
            .filter_map(|(kind, age_seconds)| {
                let w = f64::from(event_weight_milli(kind)?) / 1000.0;
                let age_hours = (age_seconds.max(0) as f64) / 3600.0;
                Some(w * (-age_hours / TREND_HALF_LIFE_HOURS).exp2())
            })
            .sum()
    }

    /// Saturating velocity → local per-mille: `round(1000·v / (v + K))`.
    /// Monotone, 0 at v=0, 500‰ at v=K, asymptotically 1000‰.
    pub fn local_trend_pm(v: f64) -> i64 {
        if v <= 0.0 {
            return 0;
        }
        (1000.0 * v / (v + TREND_SATURATION_V)).round() as i64
    }

    /// The bounded distinct-peer presence ramp (frame carve-out D7):
    /// `min(cap, floor(100·log2(1+n)))` — n=1 → 100‰, n=3 → 200‰, n≥7 → the
    /// 300‰ cap. Presence-only: each distinct peer is one bit; claimed
    /// magnitudes buy nothing (the hostile-signer rule).
    pub fn peer_ramp_pm(distinct_peers: u32) -> i64 {
        if distinct_peers == 0 {
            return 0;
        }
        let ramp = (100.0 * f64::from(1 + distinct_peers).log2()).floor() as i64;
        ramp.min(TREND_PEER_CAP_PM)
    }

    /// The composed `trending` row value: local term plus the peer ramp,
    /// capped at 1000‰.
    pub fn trend_score_pm(local_pm: i64, distinct_peers: u32) -> i64 {
        (local_pm + peer_ramp_pm(distinct_peers)).min(1000)
    }

    /// THE export k-gate (`trending.md` § The k-anonymity gate): every path
    /// that lets a local trend entry cross a nest boundary — the federation
    /// export and nothing else today — consumes this gate, never the table.
    /// `Some(())` at ≥ [`TREND_MIN_ENGAGERS`] distinct local engagers.
    pub fn exposed_trend_entry(distinct_local_engagers: u32) -> Option<()> {
        (distinct_local_engagers >= TREND_MIN_ENGAGERS).then_some(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn weights_match_the_ratified_table() {
            assert_eq!(event_weight_milli("like"), Some(1000));
            assert_eq!(event_weight_milli("reply"), Some(1500));
            assert_eq!(event_weight_milli("repost"), Some(2000));
            assert_eq!(event_weight_milli("quote"), Some(2000));
            // Views deliberately excluded in v1; unknown kinds contribute 0.
            assert_eq!(event_weight_milli("view"), None);
            assert_eq!(event_weight_milli("unlike"), None);
        }

        #[test]
        fn velocity_decays_by_half_each_half_life() {
            let fresh = local_velocity([("like", 0)].into_iter());
            assert!((fresh - 1.0).abs() < 1e-9);
            let aged = local_velocity([("like", 6 * 3600)].into_iter());
            assert!((aged - 0.5).abs() < 1e-9, "one half-life → half weight");
            // Clock skew (a future event) never amplifies.
            let future = local_velocity([("like", -3600)].into_iter());
            assert!((future - 1.0).abs() < 1e-9);
            // Kinds sum with their weights.
            let mixed = local_velocity([("like", 0), ("repost", 0), ("view", 0)].into_iter());
            assert!((mixed - 3.0).abs() < 1e-9, "1.0 + 2.0 + excluded view");
        }

        #[test]
        fn saturation_curve_shape() {
            assert_eq!(local_trend_pm(0.0), 0);
            assert_eq!(local_trend_pm(-1.0), 0);
            assert_eq!(local_trend_pm(TREND_SATURATION_V), 500);
            // Monotone, asymptotically below 1000.
            assert!(local_trend_pm(5.0) < local_trend_pm(12.0));
            assert!(local_trend_pm(10_000.0) <= 1000);
            assert!(local_trend_pm(10_000.0) >= 990);
        }

        #[test]
        fn peer_ramp_is_bounded_presence_only() {
            assert_eq!(peer_ramp_pm(0), 0);
            assert_eq!(peer_ramp_pm(1), 100);
            assert_eq!(peer_ramp_pm(3), 200);
            assert_eq!(peer_ramp_pm(6), 280);
            assert_eq!(peer_ramp_pm(7), TREND_PEER_CAP_PM);
            assert_eq!(peer_ramp_pm(10_000), TREND_PEER_CAP_PM);
        }

        #[test]
        fn score_composes_and_caps() {
            // Peer-only ceiling: far below saturation.
            assert_eq!(trend_score_pm(0, 10_000), TREND_PEER_CAP_PM);
            assert_eq!(trend_score_pm(500, 3), 700);
            assert_eq!(trend_score_pm(900, 7), 1000, "capped at 1000");
        }

        #[test]
        fn export_gate_opens_exactly_at_k() {
            assert_eq!(exposed_trend_entry(0), None);
            assert_eq!(exposed_trend_entry(2), None);
            assert_eq!(exposed_trend_entry(3), Some(()));
            assert_eq!(exposed_trend_entry(100), Some(()));
        }
    }
}

/// The `engagement` factor — the nest-computed cumulative-engagement scalar,
/// the decay-free sibling of [`trends`].
///
/// It rides `content_meta.score` (frame § Composition — "the single
/// nest-computed recency/engagement score becomes one factor among many";
/// [`factor::ENGAGEMENT`]), read by the composed feed as one weighted term.
/// **It is deliberately NOT a [`content_scores`] bus row** — it is a plain local
/// SQLite REAL in `[0, 1)`, which the composed feed multiplies by the caller's
/// weight directly (`db/feeds.rs`: `weight_permille · cm.score`, the bus-factor
/// term's `/1000` already folded in). So — unlike every bus factor — its value
/// is an f64, never a dag-cbor per-mille integer (it never crosses a wire).
///
/// **The pair with [`trends`].** Both saturate the *same* weighted explicit-act
/// signal (one shared weight table, [`trends::event_weight_milli`]), but:
///   - `trending` = **decayed velocity** — *what is rising right now*. Its value
///     drifts with wall-clock, so a periodic sweep re-decays live rows.
///   - `engagement` = **cumulative saturation** — *all-time interaction weight*.
///     A pure function of the current counters: it changes only when a count
///     changes, so it needs no sweep and is recomputed at the single counter
///     choke point (`db::engagement::increment/decrement_engagement_count`),
///     where it cannot drift from the counts.
///
/// This is the frame's decomposition of the retired pre-frame monolith
/// (`fauna_core::score::compute_score`, deleted 2026-07-13): its freshness-decay
/// term is now `trending`; its label-confidence + trust terms are separate
/// composition bus factors (`report:spam`, `labeler:<hex>`, …, each weighted on
/// its own axis); its personalization term is a sealed tier-1 client-side factor
/// (`topic-factors.md`). Only the engagement-counts term legitimately remained a
/// nest metadata scalar — this. Decision + rationale:
/// `docs/goal/behavior/trending.md` § The engagement factor.
pub mod engagement {
    use super::trends::event_weight_milli;

    /// Cumulative-engagement half-saturation constant: a post whose weighted
    /// explicit-act total reaches this value scores 500‰ (half the ceiling).
    /// **Refutable** — sized so an ordinarily-popular post lands mid-scale, well
    /// above the long tail. Distinct from [`super::trends::TREND_SATURATION_V`]
    /// (which saturates a *decayed velocity*, not a cumulative count), so the two
    /// factors keep independent calibration.
    pub const ENGAGEMENT_SATURATION: f64 = 50.0;

    /// The cumulative weighted engagement of a post: `Σ w(kind)·count(kind)` over
    /// the explicit-act counters, using the shared trend weight table (like 1.0 /
    /// reply 1.5 / repost 2.0 / quote 2.0; views excluded). **No decay** — every
    /// past interaction counts at full weight (that is the whole difference from
    /// [`super::trends::local_velocity`]). Negative counts (never expected — the
    /// counters clamp at 0) contribute nothing.
    pub fn weighted_engagement(like: i64, reply: i64, repost: i64, quote: i64) -> f64 {
        let term = |kind, count: i64| {
            f64::from(event_weight_milli(kind).unwrap_or(0)) / 1000.0 * count.max(0) as f64
        };
        term("like", like) + term("reply", reply) + term("repost", repost) + term("quote", quote)
    }

    /// The `engagement` factor value in **[0, 1)**: a Michaelis–Menten saturation
    /// of [`weighted_engagement`], `e / (e + K)`. Monotone in every count, 0 at no
    /// engagement, 0.5 at `e = K` ([`ENGAGEMENT_SATURATION`]), asymptotically 1.
    /// Persisted on `content_meta.score`; see the module doc for why this is an
    /// f64 rather than the bus's per-mille integer.
    pub fn engagement_score(like: i64, reply: i64, repost: i64, quote: i64) -> f64 {
        let e = weighted_engagement(like, reply, repost, quote);
        if e <= 0.0 {
            return 0.0;
        }
        e / (e + ENGAGEMENT_SATURATION)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn weights_are_the_shared_trend_table() {
            // One like is 1.0, one repost is 2.0 — same table as `trends`.
            assert!((weighted_engagement(1, 0, 0, 0) - 1.0).abs() < 1e-9);
            assert!((weighted_engagement(0, 1, 0, 0) - 1.5).abs() < 1e-9);
            assert!((weighted_engagement(0, 0, 1, 0) - 2.0).abs() < 1e-9);
            assert!((weighted_engagement(0, 0, 0, 1) - 2.0).abs() < 1e-9);
            // Sums across kinds; a repost outweighs a like.
            assert!((weighted_engagement(10, 5, 3, 0) - (10.0 + 7.5 + 6.0)).abs() < 1e-9);
        }

        #[test]
        fn score_saturates_in_unit_interval() {
            assert_eq!(engagement_score(0, 0, 0, 0), 0.0);
            // 0.5 exactly at the half-saturation weighted total (50 likes).
            assert!((engagement_score(50, 0, 0, 0) - 0.5).abs() < 1e-9);
            // Monotone increasing, and always strictly below 1.
            assert!(engagement_score(5, 0, 0, 0) < engagement_score(20, 0, 0, 0));
            assert!(engagement_score(1_000_000, 0, 0, 0) < 1.0);
            assert!(engagement_score(1_000_000, 0, 0, 0) > 0.99);
        }

        #[test]
        fn no_time_decay_unlike_trends() {
            // Two posts with identical cumulative counts score identically,
            // regardless of when the engagement happened — engagement has no age
            // input at all (that is `trends`' job).
            assert_eq!(engagement_score(10, 2, 1, 0), engagement_score(10, 2, 1, 0));
            // A stray negative (should never occur) is floored, not amplified.
            assert_eq!(engagement_score(-5, 0, 0, 0), 0.0);
        }
    }
}

/// The canonical *current* version of every built-in scoring factor — the
/// deployment-wide model-version-registry seed the content-at-rest re-score
/// drain compares each per-content `scorer_version` watermark against
/// (capability-mediated content-processing design § 2.5; consumer: the nest's
/// `model_versions` table + `db::model_versions::content_scores_behind`
/// obligation scan). One entry per [`factor`] constant; the four `auth_*`
/// factors deliberately share the single [`scorer_version::AUTH`] source
/// constant (they are one authentication scorer emitting four factor rows).
///
/// This is the one authoritative list of "which built-in factors exist and at
/// what version", derived from the `factor::*` + `scorer_version::*` constants
/// so adding a factor updates one place. A code deploy that bumps a
/// `scorer_version::*` constant raises the corresponding registry row on the
/// next nest boot (the nest's `seed_builtin_model_versions` reconcile), which is
/// what makes the re-score obligation fire for content scored under the old
/// version. Community/admin labelers register their own versions at runtime;
/// this list is only the built-ins.
pub fn builtin_factor_versions() -> [(&'static str, u32); 7] {
    [
        (factor::SPAM, scorer_version::SPAM),
        (factor::CLAMAV, scorer_version::CLAMAV),
        (factor::RSPAMD, scorer_version::RSPAMD),
        (factor::AUTH_SPF, scorer_version::AUTH),
        (factor::AUTH_DKIM, scorer_version::AUTH),
        (factor::AUTH_DMARC, scorer_version::AUTH),
        (factor::AUTH_ARC, scorer_version::AUTH),
    ]
}

/// The built-in mail perimeter's bus rows — the ONE per-kind verdict → row
/// mapping (`content-scoring.md` § The scoring-metadata bus, the contract
/// phase). The Go MTA calls it over UniFFI at the ingest edge, once per
/// recipient (the spam score is per-recipient once the unlisted-recipient
/// penalty applies), and sends the rows on `IngestInboundMailRequest.scores`
/// beside the per-kind detail fields; the nest stores the rows it is sent and
/// derives nothing. The submission twin (a Fauna recipient of a locally
/// submitted message) calls it with the same inputs its per-kind fields carry,
/// so its bus rows and its detail records agree.
///
/// One row per factor that actually produced a verdict: a scorer that did not
/// run (rspamd disabled, ClamAV bypass or error, an indeterminate auth verdict)
/// emits no row — `spam` always does, since the perimeter spam gate runs for
/// every delivery and its `0` is "scored ham". Units: `spam` and `rspamd` are
/// milli-points on the 0–15 spam scale (the unit `RspamdScore::scaled_milli`
/// already carries); verdict factors map Pass→0, SPF SoftFail→500, a
/// definitive Fail→1000, ClamAV Infected→1000 — the crude uniform summary. The
/// full detail (signature, rule breakdown, DMARC policy, …) stays in the
/// per-kind fields and the columns they populate, which are the detail record
/// the bus summarizes, not a shape awaiting retirement.
///
/// `spam_score_milli` is the combined perimeter score in milli-points
/// (`fauna_mail::spam::combined_spam_score_milli`, penalty applied), NOT the
/// floored 0–15 `spam_score` points the wire's per-kind field carries: the
/// expand-phase nest-side derivation wrote those points into the per-mille
/// row, a unit nothing else on the bus used. The contract phase corrected it
/// without a [`scorer_version::SPAM`] bump because no row of the points era
/// exists at rest anywhere (`version-compatibility.md` § Dimension 2, the
/// 2026-09-24 baseline reset) and no reader consumed the value.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn perimeter_mail_score_rows(
    spam_score_milli: i32,
    clamav: &ClamavVerdict,
    rspamd: Option<RspamdScore>,
    verdicts: &AuthVerdicts,
) -> Vec<ScoreEntry> {
    let mut rows = Vec::with_capacity(7);
    // Tier 1: the spam factor is the user's model in the frame's tier
    // assignment, even though the perimeter half of it is deployment-wide.
    rows.push(ScoreEntry {
        factor: factor::SPAM.to_string(),
        score: i64::from(spam_score_milli),
        tier: TIER_USER,
        scorer_version: scorer_version::SPAM,
    });
    match clamav {
        ClamavVerdict::Clean => rows.push(ScoreEntry {
            factor: factor::CLAMAV.to_string(),
            score: 0,
            tier: TIER_ADMIN,
            scorer_version: scorer_version::CLAMAV,
        }),
        ClamavVerdict::Infected { .. } => rows.push(ScoreEntry {
            factor: factor::CLAMAV.to_string(),
            score: 1000,
            tier: TIER_ADMIN,
            scorer_version: scorer_version::CLAMAV,
        }),
        // Error tempfails at the perimeter (forensic-only); an oversize bypass
        // or a door that never invokes the gate (the submission twin, a
        // disabled scanner) means the message was never scanned. None is a
        // verdict: a scorer that did not run emits no row.
        ClamavVerdict::Error { .. }
        | ClamavVerdict::BypassedOversize
        | ClamavVerdict::NotScanned => {}
    }
    if let Some(r) = rspamd {
        rows.push(ScoreEntry {
            factor: factor::RSPAMD.to_string(),
            score: i64::from(r.scaled_milli),
            tier: TIER_ADMIN,
            scorer_version: scorer_version::RSPAMD,
        });
    }
    let auth = |name: &str, score: i64| ScoreEntry {
        factor: name.to_string(),
        score,
        tier: TIER_ADMIN,
        scorer_version: scorer_version::AUTH,
    };
    match verdicts.spf {
        SpfVerdict::Pass => rows.push(auth(factor::AUTH_SPF, 0)),
        SpfVerdict::SoftFail => rows.push(auth(factor::AUTH_SPF, 500)),
        SpfVerdict::Fail => rows.push(auth(factor::AUTH_SPF, 1000)),
        SpfVerdict::None | SpfVerdict::Neutral | SpfVerdict::PermError | SpfVerdict::TempError => {}
    }
    match &verdicts.dkim {
        DkimVerdict::Pass => rows.push(auth(factor::AUTH_DKIM, 0)),
        DkimVerdict::Fail { .. } => rows.push(auth(factor::AUTH_DKIM, 1000)),
        DkimVerdict::None
        | DkimVerdict::Neutral
        | DkimVerdict::PermError
        | DkimVerdict::TempError => {}
    }
    match &verdicts.dmarc {
        DmarcVerdict::Pass => rows.push(auth(factor::AUTH_DMARC, 0)),
        DmarcVerdict::Fail { .. } => rows.push(auth(factor::AUTH_DMARC, 1000)),
        DmarcVerdict::None | DmarcVerdict::PermError | DmarcVerdict::TempError => {}
    }
    match verdicts.arc {
        ArcVerdict::Pass => rows.push(auth(factor::AUTH_ARC, 0)),
        ArcVerdict::Fail => rows.push(auth(factor::AUTH_ARC, 1000)),
        ArcVerdict::None | ArcVerdict::PermError | ArcVerdict::TempError => {}
    }
    rows
}

/// The canonical scoring-bus factor a community WASM labeler writes: the literal
/// prefix `"labeler:"` followed by the lowercase-hex of its `algorithm_id` (the
/// labeler's [`ActorId`]). This is the single source of truth linking a
/// published labeler to its `model_versions.model_kind` /
/// `content_scores.factor` (labeler-registry design § 4 "The factor namespace").
///
/// It can never collide with a built-in [`factor`] constant: those are bare
/// words (`"clamav"`, `"spam"`, …) with no `':'`, while a labeler factor is
/// always `"labeler:<64-hex>"`. So `builtin_factor_versions()` (seeded at boot)
/// and the community registry (upserted at subscribe) share `model_versions`
/// cleanly. `ScoreEntry.tier` for such a row is always [`TIER_COMMUNITY`].
pub fn labeler_factor(algorithm_id: &ActorId) -> String {
    format!("{LABELER_FACTOR_PREFIX}{}", algorithm_id.to_hex())
}

/// The reserved factor-key namespace of a published community labeler.
const LABELER_FACTOR_PREFIX: &str = "labeler:";

/// Whether `factor` is in the reserved `labeler:` namespace. A pure namespace
/// check, the [`is_topic_factor`] shape; [`labeler_factor_id`] is the strict
/// validator/parser.
pub fn is_labeler_factor(factor: &str) -> bool {
    factor.starts_with(LABELER_FACTOR_PREFIX)
}

/// The labeler `algorithm_id` a `labeler:<64-hex>` composition key names, or
/// `None` if `factor` is not a well-formed key in that namespace.
///
/// The inverse of [`labeler_factor`], and the read a **subscriber's client**
/// needs: a composition carries factor *keys*, but fetching the artifact to
/// score with (`fauna.labelers.inspect`) takes the id those keys encode.
pub fn labeler_factor_id(factor: &str) -> Option<ActorId> {
    let hex = factor.strip_prefix(LABELER_FACTOR_PREFIX)?;
    let mut id = [0u8; 32];
    hex::decode_to_slice(hex, &mut id).ok()?;
    Some(ActorId(id))
}

/// The reserved factor-key namespace for the user's private trainable topic
/// factors (`docs/goal/behavior/topic-factors.md` § The model).
const TOPIC_FACTOR_PREFIX: &str = "topic:";

/// The canonical composition factor key of a trainable topic factor: the
/// literal prefix `"topic:"` followed by the lowercase-hex of its 16-byte
/// random id, minted at factor creation (`docs/goal/behavior/topic-factors.md`
/// § The model). Like [`labeler_factor`] it can never collide with a bare
/// built-in [`factor`] constant (those carry no `':'`). The factor's display
/// name lives only in the user's sealed registry — the nest sees just this
/// opaque key inside compositions.
///
/// A **sealed tier-1** factor: it writes no nest-side `content_scores` rows
/// (its nest composition term is always 0 — the sealed-factor seam) and is
/// deliberately NOT in [`builtin_factor_versions`], the [`factor::MUTED_KEYWORDS`]
/// precedent — a model-version-registry entry would create re-score
/// obligations no capability-holder can serve.
pub fn topic_factor(id: &[u8; 16]) -> String {
    format!("{TOPIC_FACTOR_PREFIX}{}", hex::encode(id))
}

/// Whether `factor` is in the reserved `topic:` namespace (a sealed tier-1
/// trained-topic key — zero nest-side term, client-side compose;
/// `docs/goal/behavior/topic-factors.md` § Scoring). A pure namespace check;
/// [`topic_factor_id`] is the strict validator/parser.
pub fn is_topic_factor(factor: &str) -> bool {
    factor.starts_with(TOPIC_FACTOR_PREFIX)
}

/// The reserved key namespace for the user's BackupKey-sealed engagement-cue
/// rollup (`cues:v1`), stored verbatim-opaque in `personalization_models` purely
/// for cross-device continuity + reinstall survival
/// (`docs/goal/behavior/engagement-cues.md` § Seal + home). It rides the sealed
/// personalization-model table + wire but is **not** a composition factor: it
/// carries no scoring term, so [`is_topic_factor`] deliberately excludes it and
/// `FeedManager` never folds it — only [`is_personalization_model_factor`] (the
/// nest's envelope validation) accepts it beside `topic:`.
const CUES_MODEL_PREFIX: &str = "cues:";

/// The v1 factor key of the user's BackupKey-sealed engagement-cue rollup on the
/// personalization-model wire (`engagement-cues.md` § Seal + home). One constant
/// so the client's put/fetch/delete and the nest's conformance test name the
/// identical key — the rollup is a single mutable record per actor, not a
/// namespace. The `v1` suffix versions the *rollup layout*; a future layout is a
/// new key added beside this one (additive), never a break of this one.
pub const CUES_ROLLUP_FACTOR_V1: &str = "cues:v1";

/// Whether `factor` is an accepted key on the sealed personalization-model wire
/// (`fauna.personalization.model.{fetch,put,delete}`): the `topic:` trained-model
/// namespace OR the `cues:` engagement-cue rollup namespace. The nest's
/// envelope-only put/fetch/delete validation consumes this — the sealed blob
/// itself stays opaque. Additive by design: future sealed model kinds added
/// *beside* these (per the frame's portfolio principle) widen it.
/// `docs/goal/behavior/engagement-cues.md` § Seal + home ("the nest-side put
/// validation extends its accepted prefix set from `topic:` to `topic: | cues:`").
pub fn is_personalization_model_factor(factor: &str) -> bool {
    factor.starts_with(TOPIC_FACTOR_PREFIX) || factor.starts_with(CUES_MODEL_PREFIX)
}

/// Strict inverse of [`topic_factor`]: the 16-byte id of a canonical
/// `topic:<32-lowercase-hex>` key, or `None` for anything else (wrong prefix,
/// wrong length, uppercase or non-hex digits — no trimming, no leniency; only
/// keys this module minted parse).
pub fn topic_factor_id(factor: &str) -> Option<[u8; 16]> {
    let hex_part = factor.strip_prefix(TOPIC_FACTOR_PREFIX)?;
    if hex_part.len() != 32 {
        return None;
    }
    let bytes = hex::decode(hex_part).ok()?;
    // `hex::decode` also accepts uppercase digits; the canonical key is
    // lowercase-only, so require the round-trip to reproduce the input.
    if hex::encode(&bytes) != hex_part {
        return None;
    }
    bytes.try_into().ok()
}

/// Engagement-cue derivation thresholds + rollup caps (`engagement-cues.md`
/// § Cue vocabulary & derivation, § At rest). Every value is hard-coded (bucket
/// 1 — no human ever chooses them) and lives here, in shared Rust, so the two
/// client-side verdicts (`watch-complete` / `skip`) can never drift per platform.
///
/// **The split the module makes explicit (boundary revised 2026-07-29 —
/// `engagement-cues.md` § Cue vocabulary & derivation):** only the raw geometry
/// *probe* (each app's intersection/visibility and media-playback APIs) is
/// platform glue; the sampling *bookkeeping* above it (credit arithmetic,
/// hold/leave policy, noise floor) is the shared `fauna_feed` `CueTracker`, and
/// every threshold either compares against is one of these constants — so
/// "≥ 75 % visible for 8 s" means the identical thing everywhere. Fractions are
/// integer per-mille (the bus/score convention throughout this file; the float
/// ban is a wire rule, but per-mille keeps the derivation exact and
/// allocation-free regardless).
pub mod cues {
    /// Media `watch-complete`: playback reached at least this fraction of the
    /// item's duration. The goal doc's `CUE_COMPLETE_FRACTION` (0.85), per-mille.
    pub const CUE_COMPLETE_FRACTION_PM: u32 = 850;

    /// Non-media `watch-complete`: the item was held at ≥ [`CUE_LONG_DWELL_VISIBLE_PM`]
    /// visibility for at least this long. The goal doc's `CUE_DWELL_LONG_MS`.
    pub const CUE_DWELL_LONG_MS: u64 = 8_000;

    /// The visibility fraction (per-mille) that counts as "substantially on
    /// screen" for the long-dwell `watch-complete` gate — the goal doc's "≥ 75 %
    /// visible". Each platform's visibility observer buckets long-dwell time at
    /// this threshold, so the 8 s gate is measured identically everywhere.
    pub const CUE_LONG_DWELL_VISIBLE_PM: u32 = 750;

    /// `skip`: the item was at least [`CUE_SKIP_VISIBLE_PM`] visible for *less
    /// than* this long before the user scrolled on. The goal doc's `CUE_SKIP_MS`.
    pub const CUE_SKIP_MS: u64 = 1_200;

    /// The visibility fraction (per-mille) an item must have *reached* to be a
    /// `skip` candidate at all — the goal doc's "≥ 50 % visible". Below this the
    /// item was never really on screen, so scrolling past it is no judgment.
    pub const CUE_SKIP_VISIBLE_PM: u32 = 500;

    // ── Capture-side bookkeeping (the shared `fauna_feed::CueTracker`) ────────
    // The three constants below govern *sampling*, not derivation. They moved
    // here from four independent per-app copies with the 2026-07-29 boundary
    // revision (`engagement-cues.md` § Cue vocabulary & derivation): every shell
    // had hand-written the identical values, and two of them had already drifted
    // on the clock that feeds the credit.

    /// Sampling cadence. Fine enough to catch a deliberate dwell precisely
    /// ([`CUE_SKIP_MS`] = 1200 ms spans ~5 ticks) without measurable idle cost.
    /// A capture shell reads it through the tracker face and never re-declares
    /// it — the tick rate and the dwell thresholds are one calibration.
    pub const CUE_SAMPLE_INTERVAL_MS: u64 = 250;

    /// Ceiling on the elapsed time one sample may credit as dwell. A stalled
    /// event loop (system suspend, a long blocking dialog, a backgrounded app)
    /// must not credit its whole gap as on-screen dwell — the card wasn't being
    /// watched while nothing painted.
    pub const CUE_MAX_SAMPLE_CREDIT_MS: u64 = 1_000;

    /// The single-sample noise floor: an exposure confirmed by fewer than this
    /// many samples is dropped at emit. An image-load layout shift flashes cards
    /// through the viewport for one tick, and reporting those as exposures
    /// fabricates skips the user never made (the engine's burst gate cannot
    /// catch an isolated flash mid-quiet-dwell).
    pub const CUE_MIN_VISIBLE_SAMPLES: u32 = 2;

    /// Burst-suppression gap: consecutive exposures arriving closer together than
    /// this are a fast-scroll flick, not reading, and derive **no** `skip`
    /// (`engagement-cues.md` § Cue vocabulary — "a fast-scroll burst derives
    /// nothing — flick-throughs are not judgments"). `watch-complete` is naturally
    /// burst-immune (you cannot complete a video or dwell 8 s mid-flick), so only
    /// `skip` consults this. Not named in the goal doc: it is the concrete
    /// realization of the doc's "normal reading pace" clause — a bucket-1 constant
    /// introduced with this implementation and ratified into § Cue vocabulary.
    pub const CUE_BURST_MIN_GAP_MS: u64 = 400;

    /// Sealed cue-rollup capacity (`engagement-cues.md` § At rest): at most this
    /// many per-item entries are retained; past the cap the entry with the oldest
    /// `last_at` is evicted.
    pub const CUE_ROLLUP_MAX_ITEMS: usize = 4_096;

    /// Sealed cue-rollup put debounce, in seconds (`engagement-cues.md` § At
    /// rest): the client coalesces rollup mutations and puts at most once per this
    /// interval (or on background/close), never once per scroll.
    pub const CUE_PUT_DEBOUNCE_S: u64 = 120;

    /// **Layer-A weak-engagement weight**, per-mille (`topic-factors.md`
    /// § Training signals — the ratified v2 weighting shape). A derived cue
    /// verdict (`watch-complete` / `skip`) trains a *weak* example into each
    /// `learn_from_engagement`-on trained factor: the `TopicModel`'s integer
    /// engagement counters are folded into the Bernoulli posterior at score time
    /// as `explicit + engagement · CUE_ENGAGEMENT_WEIGHT_PM/1000` effective
    /// documents (so `200` = 0.2 documents per engagement event — an explicit
    /// *more like this* tap is worth five watch-completes). Bucket-1 constant: a
    /// human never chooses it, and a model with zero engagement counts scores
    /// **bit-identically** to a pre-v2 model regardless of its value.
    pub const CUE_ENGAGEMENT_WEIGHT_PM: u32 = 200;
}

/// Map a labeler's `label()` output ([`Label`]s) to its single bus
/// [`ScoreEntry`] (labeler-registry design § 6, D6 — "Output → bus"). The
/// **primary** label is the highest-confidence one; its `confidence` ∈ [0,1]
/// becomes the per-mille score `round(confidence * 1000)`. An empty `labels`
/// (the labeler detected nothing) maps to score `0`. `factor` is
/// [`labeler_factor`] of the labeler; `scorer_version` is the
/// subscribed/registered version. The row is always [`TIER_COMMUNITY`].
///
/// This is the float→int boundary the wire requires (canonical dag-cbor forbids
/// floats, and [`Label::confidence`] is one). v1 is one-labeler-one-factor;
/// a multi-channel labeler emitting several bus factors is a future additive
/// extension (design D6: each becomes a `labeler:<id>#<channel>` factor).
pub fn labels_to_score_entry(labels: &[Label], factor: String, scorer_version: u32) -> ScoreEntry {
    // `confidence` is an untrusted WASM-module output; a malicious or buggy
    // labeler can emit out-of-range or NaN values. `fold(0.0, f64::max)` already
    // drops NaN (and floors the accumulator at 0.0, so it is never NaN), and the
    // final `clamp` bounds the result to the documented [0,1] domain — so the
    // per-mille score stays in [0,1000] and no downstream ranking/bus consumer
    // sees a factor row outside the contract.
    let primary_confidence = labels
        .iter()
        .map(|l| l.confidence)
        .fold(0.0_f64, f64::max)
        .clamp(0.0, 1.0);
    let score = (primary_confidence * 1000.0).round() as i64;
    ScoreEntry {
        factor,
        score,
        tier: TIER_COMMUNITY,
        scorer_version,
    }
}

/// The labeler artifact kinds (labeler-registry design Block A, D8). An
/// additive envelope axis beside `content_kind` — never a field of the signed
/// `AlgorithmLabeler` (that shape stays frozen); absent/empty on the wire ⇒
/// [`artifact_kind::WASM`], the original kind.
pub mod artifact_kind {
    /// An executable WASM `label()` module, run at a capability holder.
    pub const WASM: &str = "wasm";
    /// A curated content list ([`super::LabelerListArtifact`]) — pure
    /// membership lookup; no execution, no holder, no drain. The nest itself
    /// materializes its `content_scores` rows (frame § Tier-3 artifact kinds).
    pub const LIST: &str = "list";
    /// A scrubbed Bernoulli n-gram count table
    /// ([`super::TextModelArtifact`]) — pure data, not code, so no sandbox and
    /// no fuel bounds. Unlike a List it **generalizes to unseen content**, and
    /// unlike both other kinds it is evaluated **at the subscriber's client,
    /// never the nest**: no `content_scores` rows, no `model_versions` entry,
    /// no drain, no holder, no grant (frame § Tier-3 artifact kinds).
    pub const TEXT_MODEL: &str = "text-model";
}

/// Maximum entries a List labeler artifact may carry (design Block A, D9 —
/// bounds the nest's synchronous materialization work per subscribe/republish;
/// refutable constant). The 1 MiB artifact byte cap (`MAX_LABELER_WASM_BYTES`,
/// nest-side) bounds the bytes; this bounds the row count.
pub const MAX_LABELER_LIST_ENTRIES: usize = 16_384;

/// One entry of a List labeler artifact: a 32-byte content id (the raw hash
/// form `content_meta.content_id` / `content_scores.content_id` carry — the
/// CID digest without the multiformat prefix) and its per-mille score.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListEntry {
    /// The 32-byte content id the score attaches to.
    pub content_id: serde_bytes::ByteBuf,
    /// Per-mille score ∈ [0, 1000] — the same range every bus factor emits
    /// (the F5-clamped Wasm output range). Filter/boost *sign* comes from
    /// composition weights (frame § Composition), never from the factor value.
    pub score: i64,
}

/// Maximum length (in `char`s) of a List artifact's publisher-chosen `name`.
/// The sealed registry name has no cap — it is private and self-inflicted —
/// but a published name is **public**, so it is bounded like every other
/// cross-user string.
pub const MAX_LABELER_LIST_NAME_LEN: usize = 128;

/// The List labeler artifact (design Block A, D9): a signed, versioned,
/// size-bounded **dag-cbor** map `content_id → score`, carried in the publish
/// wire's `wasm_bytes` field (read "artifact bytes"). Canonical form —
/// enforced by [`validate_list_artifact`] at publish — is strictly ascending
/// by `content_id` with no duplicates, so equal lists have equal bytes and
/// the metadata hash binding is meaningful. Evaluation is pure membership
/// lookup; integer scores fit the house codec (dag-cbor forbids floats).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabelerListArtifact {
    pub entries: Vec<ListEntry>,
    /// The publisher-chosen public display name (`topic-factors.md` §
    /// Publishing a trained factor). It rides **inside the artifact** rather
    /// than on the wire, which is what lets that section promise both a
    /// publisher-chosen name and "zero new wire": the signed
    /// [`AlgorithmLabeler`] carries no name and its shape is frozen, and its
    /// signature is checked by *re-encoding* the metadata — so a field there
    /// would make every older nest reject every named labeler. Here the name
    /// is instead bound for free by `wasm_hash`, which hashes these raw bytes
    /// with no re-encode, and an older nest simply ignores the unknown field
    /// (no `deny_unknown_fields`) and materializes the entries as always.
    ///
    /// `skip_serializing_if` keeps an unnamed list **byte-identical** to the
    /// pre-name shape, so artifacts published before this axis existed still
    /// hash — and therefore verify — unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Why a List labeler artifact failed [`validate_list_artifact`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ListArtifactError {
    /// The bytes are not a dag-cbor `LabelerListArtifact`.
    #[error("list artifact decode failed: {0}")]
    Decode(String),
    /// More than [`MAX_LABELER_LIST_ENTRIES`] entries.
    #[error("list artifact has {0} entries (max {MAX_LABELER_LIST_ENTRIES})")]
    TooManyEntries(usize),
    /// An entry's `content_id` is not exactly 32 bytes.
    #[error("list artifact entry {index} content_id is {len} bytes (expected 32)")]
    BadContentIdLen { index: usize, len: usize },
    /// An entry's score is outside [0, 1000] per-mille.
    #[error("list artifact entry {index} score {score} outside [0,1000]")]
    ScoreOutOfRange { index: usize, score: i64 },
    /// Entries are not strictly ascending by `content_id` (unsorted or
    /// duplicate) — the canonical form publish requires.
    #[error("list artifact entries not strictly ascending at index {0}")]
    NotStrictlyAscending(usize),
    /// The publisher-chosen name exceeds [`MAX_LABELER_LIST_NAME_LEN`] chars.
    #[error("list artifact name is {0} chars (max {MAX_LABELER_LIST_NAME_LEN})")]
    NameTooLong(usize),
    /// The name is present but blank — distinct from *absent*, which is the
    /// valid unnamed list. A publisher who submits whitespace has made an
    /// error worth surfacing, not an unnamed list.
    #[error("list artifact name is present but blank")]
    BlankName,
}

/// Decode + validate a List labeler artifact's canonical form (design Block A,
/// D9/D10): dag-cbor, ≤ [`MAX_LABELER_LIST_ENTRIES`] entries, 32-byte ids,
/// scores ∈ [0,1000], strictly ascending by `content_id` (sorted, deduped).
/// Shared by the nest publish gate and any client that pre-validates or
/// renders the list for inspect-before-subscribe.
pub fn validate_list_artifact(bytes: &[u8]) -> Result<LabelerListArtifact, ListArtifactError> {
    let artifact: LabelerListArtifact = crate::encoding::canonical_decode(bytes)
        .map_err(|e| ListArtifactError::Decode(e.to_string()))?;
    if let Some(name) = &artifact.name {
        if name.trim().is_empty() {
            return Err(ListArtifactError::BlankName);
        }
        let chars = name.chars().count();
        if chars > MAX_LABELER_LIST_NAME_LEN {
            return Err(ListArtifactError::NameTooLong(chars));
        }
    }
    if artifact.entries.len() > MAX_LABELER_LIST_ENTRIES {
        return Err(ListArtifactError::TooManyEntries(artifact.entries.len()));
    }
    for (index, entry) in artifact.entries.iter().enumerate() {
        if entry.content_id.len() != 32 {
            return Err(ListArtifactError::BadContentIdLen {
                index,
                len: entry.content_id.len(),
            });
        }
        if !(0..=1000).contains(&entry.score) {
            return Err(ListArtifactError::ScoreOutOfRange {
                index,
                score: entry.score,
            });
        }
        if index > 0 && artifact.entries[index - 1].content_id >= entry.content_id {
            return Err(ListArtifactError::NotStrictlyAscending(index));
        }
    }
    Ok(artifact)
}

/// Build a publishable List labeler artifact from `(content_id, score)` pairs:
/// sort ascending, collapse duplicates last-wins, validate, and canonical-
/// encode to the bytes `fauna.labelers.publish` carries in `wasm_bytes`.
///
/// The canonical form is the **builder's** obligation, not the caller's — a
/// publishing UI hands over whatever the user pruned, in whatever order it
/// rendered, possibly with an id twice; every app publishes through here
/// so none of them re-derives the sort/dedup the nest gate demands
/// ([`validate_list_artifact`]). `name` is the publisher-chosen public display
/// name (trimmed; `None` for an unnamed list).
pub fn build_list_artifact(
    name: Option<&str>,
    entries: Vec<([u8; 32], i64)>,
) -> Result<Vec<u8>, ListArtifactError> {
    let name = match name {
        Some(n) if n.trim().is_empty() => return Err(ListArtifactError::BlankName),
        Some(n) => Some(n.trim().to_string()),
        None => None,
    };
    // Sort by id, then collapse duplicates keeping the last. `sort_by_key` is a
    // stable sort, which is what makes "last wins" well-defined: it preserves
    // the caller's relative order among equal ids.
    let mut entries = entries;
    entries.sort_by_key(|a| a.0);
    entries.dedup_by(|later, earlier| {
        if later.0 == earlier.0 {
            earlier.1 = later.1;
            true
        } else {
            false
        }
    });
    let artifact = LabelerListArtifact {
        entries: entries
            .into_iter()
            .map(|(content_id, score)| ListEntry {
                content_id: serde_bytes::ByteBuf::from(content_id.to_vec()),
                score,
            })
            .collect(),
        name,
    };
    let bytes = crate::encoding::canonical_encode(&artifact)
        .map_err(|e| ListArtifactError::Decode(e.to_string()))?;
    // Validate our own output: the publisher must never learn at the nest gate
    // that the bytes it signed were out of contract.
    validate_list_artifact(&bytes)?;
    Ok(bytes)
}

// ── The `text-model` labeler artifact (v2 — `topic-factors.md` § Publishing a
// trained factor, RATIFIED 2026-08-13) ──────────────────────────────────────

/// The `TextModelArtifact` schema version this build **produces and can
/// score**. It carries the **tokenizer contract** (NFKC / UAX#29, 1/2/3-grams):
/// two artifacts at the same version were tokenized the same way, so their
/// n-grams mean the same thing.
///
/// A subscriber meeting a *higher* version treats the factor as **inert and
/// says so** (frame § Tier-3 artifact kinds) rather than scoring text its
/// tokenizer would split differently — a silent mis-score is the one outcome
/// the version exists to prevent. Note this is deliberately **not** enforced by
/// [`validate_text_model_artifact`]: see that function's compat note.
pub const TEXT_MODEL_ARTIFACT_VERSION: u16 = 1;

/// Whether **this build** implements a `text-model` artifact's tokenizer
/// contract — the one predicate behind both halves of the unknown-version
/// rule (frame § Tier-3 artifact kinds: the factor is *inert* **and says so*).
///
/// It exists as a shared function rather than an inline `!=` because the two
/// halves are enforced in different crates — the compose seam decides whether
/// to score (`fauna_feed::FeedManager::load_sealed_scorers`) and the catalog
/// row decides whether to say "needs a newer app"
/// ([`crate::format::text_model_needs_newer_app`]) — and a build whose badge
/// and whose scorer disagreed would be *worse* than either failure alone: it
/// would either promise a score it never applies, or accuse a perfectly
/// scorable artifact of being too new.
///
/// Exact equality, deliberately, not `<=`: a *lower* version is a different
/// tokenizer contract too, so scoring it would split n-grams differently just
/// as a higher one would. (No lower version exists today — v1 is the first —
/// but the rule that matters is "the contract I implement", not "not newer".)
pub fn text_model_version_supported(version: u16) -> bool {
    version == TEXT_MODEL_ARTIFACT_VERSION
}

/// Distinct-document prune floor: an n-gram may cross into a published model
/// only if it occurred in **≥ 3 distinct** included example posts, counting
/// both classes together (`more + less`).
///
/// It is an **anti-quote privacy floor, not a signal filter**, which is why the
/// counting is class-blind: nothing unique to one or two of the publisher's
/// marked posts survives, killing both single-document text reconstruction and
/// single-post identification of the user's sealed judgment. The value matches
/// the k-anonymity floor the spam-baseline aggregate already ships.
pub const TEXT_MODEL_PUBLISH_MIN_DOCS: u32 = 3;

/// Vocabulary bound — survivors are ranked by the NB's own informativeness and
/// truncated here.
///
/// Double-duty by design: the size bound **and** the review bound. Unlike the
/// List's `REVIEW_TOP_N` (which bounds endorsement of already-public ids), the
/// vocabulary **is** the disclosure, so everything that crosses must be humanly
/// reviewable — this cap is what keeps "review the whole artifact" honest.
pub const TEXT_MODEL_PUBLISH_MAX_NGRAMS: usize = 512;

/// One entry of a text-model artifact: an n-gram and its two per-class
/// **distinct-document** occurrence counts (Bernoulli — one document counts
/// once no matter how often the n-gram repeats in it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextModelNgram {
    /// The n-gram: 1, 2, or 3 tokenizer tokens joined by single spaces.
    pub ngram: String,
    /// Documents of the *more like this* class this n-gram occurred in.
    pub more: u32,
    /// Documents of the *less like this* class this n-gram occurred in.
    pub less: u32,
}

/// The `text-model` labeler artifact (`topic-factors.md` § Publishing a trained
/// factor, v2): a signed, versioned, size-bounded **dag-cbor Bernoulli n-gram
/// count table**, carried in the publish wire's `wasm_bytes` field (read
/// "artifact bytes") exactly as a List is.
///
/// It is **not a serialization of the private model** — even a scrubbed one.
/// It is rebuilt at publish time from the factor's *public, still-fetchable
/// explicit examples only*, which is what makes four leak classes die by
/// construction rather than by stripping diligence (example markers, engagement
/// counts, restricted-content text, and the private model's statistical ghosts
/// never enter). The structural pin that keeps the rebuild honest lives with
/// the scrub in `fauna-text-model`: a model trained with engagement *and*
/// restricted examples publishes **byte-identically** to its explicit-public-
/// only twin.
///
/// Canonical form — enforced by [`validate_text_model_artifact`] at publish —
/// is strictly ascending by `ngram` with no duplicates, so equal vocabularies
/// have equal bytes and the `wasm_hash` metadata binding is meaningful (the
/// List's property).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextModelArtifact {
    /// The tokenizer/schema contract ([`TEXT_MODEL_ARTIFACT_VERSION`]).
    pub version: u16,
    /// Documents of the *more like this* class the vocabulary was built from —
    /// the posterior's positive prior and half the cold-start damp's sample
    /// count.
    pub more_docs: u32,
    /// Documents of the *less like this* class.
    pub less_docs: u32,
    /// The surviving vocabulary, strictly ascending by `ngram`.
    pub ngrams: Vec<TextModelNgram>,
    /// The publisher-chosen public display name — **the List's name rules
    /// verbatim** (§ Publishing), including riding *inside* the artifact rather
    /// than on the frozen signed metadata, and `skip_serializing_if` so an
    /// unnamed model stays byte-identical to the nameless shape. See
    /// [`LabelerListArtifact::name`] for the full rationale; it is one rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Why a text-model labeler artifact failed
/// [`validate_text_model_artifact`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TextModelArtifactError {
    /// The bytes are not a dag-cbor `TextModelArtifact`.
    #[error("text-model artifact decode failed: {0}")]
    Decode(String),
    /// `version` is 0 — an unstamped artifact, not a future one.
    #[error("text-model artifact carries version 0")]
    ZeroVersion,
    /// More than [`TEXT_MODEL_PUBLISH_MAX_NGRAMS`] n-grams.
    #[error("text-model artifact has {0} n-grams (max {TEXT_MODEL_PUBLISH_MAX_NGRAMS})")]
    TooManyNgrams(usize),
    /// N-grams are not strictly ascending (unsorted or duplicate) — the
    /// canonical form publish requires.
    #[error("text-model artifact n-grams not strictly ascending at index {0}")]
    NotStrictlyAscending(usize),
    /// At a version this build's tokenizer contract covers, an entry's
    /// `ngram` is not 1-3 non-empty tokens joined by single spaces. Such an
    /// n-gram can never match any subscriber's tokenizer, so it scores
    /// nothing and is pure disclosure of the publisher's marked-post text.
    #[error("text-model artifact entry {0} n-gram is not a valid 1-3 token n-gram")]
    InvalidNgramShape(usize),
    /// An entry is zero in **both** classes: no signal, no disclosure, pure
    /// bytes.
    #[error("text-model artifact entry {0} is zero in both classes")]
    EmptyEntry(usize),
    /// An entry survived below the [`TEXT_MODEL_PUBLISH_MIN_DOCS`] prune floor.
    #[error(
        "text-model artifact entry {index} occurs in {docs} documents \
         (prune floor {TEXT_MODEL_PUBLISH_MIN_DOCS})"
    )]
    BelowPruneFloor { index: usize, docs: u32 },
    /// A `more` count exceeds `more_docs` — impossible for a per-document
    /// occurrence count.
    #[error("text-model artifact entry {index} more={more} exceeds more_docs={more_docs}")]
    MoreCountAboveDocs {
        index: usize,
        more: u32,
        more_docs: u32,
    },
    /// A `less` count exceeds `less_docs`.
    #[error("text-model artifact entry {index} less={less} exceeds less_docs={less_docs}")]
    LessCountAboveDocs {
        index: usize,
        less: u32,
        less_docs: u32,
    },
    /// The publisher-chosen name exceeds [`MAX_LABELER_LIST_NAME_LEN`] chars.
    #[error("text-model artifact name is {0} chars (max {MAX_LABELER_LIST_NAME_LEN})")]
    NameTooLong(usize),
    /// The name is present but blank (the List's distinction: *absent* is the
    /// valid unnamed artifact; whitespace is a publisher error worth surfacing).
    #[error("text-model artifact name is present but blank")]
    BlankName,
}

/// Whether `ngram` is a well-formed n-gram under the tokenizer contract every
/// version up to [`TEXT_MODEL_ARTIFACT_VERSION`] shares: 1-3 non-empty tokens
/// joined by single spaces, with no leading, trailing, or doubled space and no
/// whitespace or control character inside a token.
///
/// `fauna_text_model`'s `message_ngrams` (the only producer of a version-1
/// n-gram) never emits anything else — its tokens come from
/// `unicode_word_indices`, which by construction cannot contain whitespace —
/// so this is a shape check on out-of-contract input, not a tokenizer
/// reimplementation.
fn is_valid_ngram_shape(ngram: &str) -> bool {
    let tokens: Vec<&str> = ngram.split(' ').collect();
    if tokens.len() > 3 {
        return false;
    }
    tokens
        .iter()
        .all(|t| !t.is_empty() && !t.chars().any(|c| c.is_whitespace() || c.is_control()))
}

/// Decode + validate a text-model artifact's canonical form. Shared by the nest
/// publish gate and by any client that pre-validates or renders the vocabulary
/// for inspect-before-subscribe.
///
/// # The prune floor is enforced HERE, not only at the publisher
///
/// [`TEXT_MODEL_PUBLISH_MIN_DOCS`] is checked structurally (`more + less ≥
/// floor`), because `more`/`less` **are** the per-class distinct-document
/// counts — so the anti-quote floor is a property of the artifact rather than
/// of the publishing client's diligence. That is the whole posture of the v2
/// design: a buggy or hostile client must not be able to put a
/// single-document quote on a public registry, and the nest gate is the one
/// boundary that can refuse it.
///
/// # The version-1 n-gram shape is enforced HERE too, at a known version only
///
/// At any `version <= TEXT_MODEL_ARTIFACT_VERSION` — a contract this build
/// actually implements — every entry's `ngram` must pass `is_valid_ngram_shape`.
/// An out-of-shape n-gram (more than 3 tokens, or a token boundary a real
/// tokenizer could never produce) can never match a subscriber's tokenizer at
/// that version, so it scores nothing and is pure disclosure: a quote of the
/// publisher's marked-post text, and so evidence of which posts they marked.
/// This does not run on an unknown higher version — see the compat note below;
/// the shape is part of *this build's* contract, not a property every future
/// version must share.
///
/// # Compat note: an unknown `version` is NOT rejected
///
/// This validator runs at the **nest publish gate**, so rejecting an
/// unrecognized `version` would make an *older nest* refuse a *newer client's*
/// artifact — the bidirectional break `version-compatibility.md` forbids within
/// a major version. Structure is this function's job; interpretation belongs to
/// the subscriber's scorer, which goes **inert** on a version it cannot
/// tokenize for. (Raising the prune floor stays compatible for the same reason
/// in reverse: a higher-floor artifact passes an older, lower floor. *Lowering*
/// it would be refused by older nests — which is the correct outcome for a
/// privacy floor.)
pub fn validate_text_model_artifact(
    bytes: &[u8],
) -> Result<TextModelArtifact, TextModelArtifactError> {
    let artifact: TextModelArtifact = crate::encoding::canonical_decode(bytes)
        .map_err(|e| TextModelArtifactError::Decode(e.to_string()))?;
    if artifact.version == 0 {
        return Err(TextModelArtifactError::ZeroVersion);
    }
    if let Some(name) = &artifact.name {
        if name.trim().is_empty() {
            return Err(TextModelArtifactError::BlankName);
        }
        let chars = name.chars().count();
        if chars > MAX_LABELER_LIST_NAME_LEN {
            return Err(TextModelArtifactError::NameTooLong(chars));
        }
    }
    if artifact.ngrams.len() > TEXT_MODEL_PUBLISH_MAX_NGRAMS {
        return Err(TextModelArtifactError::TooManyNgrams(artifact.ngrams.len()));
    }
    let check_shape = artifact.version <= TEXT_MODEL_ARTIFACT_VERSION;
    for (index, entry) in artifact.ngrams.iter().enumerate() {
        if check_shape && !is_valid_ngram_shape(&entry.ngram) {
            return Err(TextModelArtifactError::InvalidNgramShape(index));
        }
        if entry.more == 0 && entry.less == 0 {
            return Err(TextModelArtifactError::EmptyEntry(index));
        }
        // Class-blind, per the floor's own definition. `EmptyEntry` above is
        // subsumed by this at the current constant but is pinned separately by
        // § Publishing (and diagnoses the degenerate case precisely), so both
        // rules stand on their own.
        let docs = entry.more.saturating_add(entry.less);
        if docs < TEXT_MODEL_PUBLISH_MIN_DOCS {
            return Err(TextModelArtifactError::BelowPruneFloor { index, docs });
        }
        if entry.more > artifact.more_docs {
            return Err(TextModelArtifactError::MoreCountAboveDocs {
                index,
                more: entry.more,
                more_docs: artifact.more_docs,
            });
        }
        if entry.less > artifact.less_docs {
            return Err(TextModelArtifactError::LessCountAboveDocs {
                index,
                less: entry.less,
                less_docs: artifact.less_docs,
            });
        }
        if index > 0 && artifact.ngrams[index - 1].ngram >= entry.ngram {
            return Err(TextModelArtifactError::NotStrictlyAscending(index));
        }
    }
    Ok(artifact)
}

/// Build a publishable text-model artifact from a scrubbed vocabulary: sort
/// ascending, collapse duplicates last-wins, validate, and canonical-encode to
/// the bytes `fauna.labelers.publish` carries in `wasm_bytes`.
///
/// The canonical form is the **builder's** obligation, not the caller's — the
/// same division [`build_list_artifact`] draws, and for the same reason: a
/// review sheet hands over whatever the user left checked, in whatever order it
/// rendered. `version` is stamped here rather than taken as a parameter — it is
/// the tokenizer contract of the build that produced the counts, never a caller
/// choice (the `updated_at` lesson).
pub fn build_text_model_artifact(
    name: Option<&str>,
    more_docs: u32,
    less_docs: u32,
    ngrams: Vec<(String, u32, u32)>,
) -> Result<Vec<u8>, TextModelArtifactError> {
    let name = match name {
        Some(n) if n.trim().is_empty() => return Err(TextModelArtifactError::BlankName),
        Some(n) => Some(n.trim().to_string()),
        None => None,
    };
    // Stable sort by n-gram, then collapse duplicates keeping the last — the
    // List builder's rule verbatim, so "last wins" is well-defined.
    let mut ngrams = ngrams;
    ngrams.sort_by(|a, b| a.0.cmp(&b.0));
    ngrams.dedup_by(|later, earlier| {
        if later.0 == earlier.0 {
            earlier.1 = later.1;
            earlier.2 = later.2;
            true
        } else {
            false
        }
    });
    let artifact = TextModelArtifact {
        version: TEXT_MODEL_ARTIFACT_VERSION,
        more_docs,
        less_docs,
        ngrams: ngrams
            .into_iter()
            .map(|(ngram, more, less)| TextModelNgram { ngram, more, less })
            .collect(),
        name,
    };
    let bytes = crate::encoding::canonical_encode(&artifact)
        .map_err(|e| TextModelArtifactError::Decode(e.to_string()))?;
    // Validate our own output: the publisher must never learn at the nest gate
    // that the bytes it signed were out of contract.
    validate_text_model_artifact(&bytes)?;
    Ok(bytes)
}

/// Ed25519 signature length (bytes) a labeler artifact's `signature` must carry —
/// the `GrantEvent::verify` convention (`grant_event.rs`).
pub const LABELER_SIGNATURE_LEN: usize = 64;

/// Why a labeler artifact failed the shared pre-trust verification
/// ([`verify_labeler_metadata`]). Typed so each boundary maps it to its own
/// error surface (the nest → `RpcError` malformed/permission-denied; the FFI
/// holder → `FfiError`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LabelerVerifyError {
    /// Declared `wasm_size` ≠ the actual module byte length.
    #[error("labeler wasm_size {declared} != actual {actual} bytes")]
    WasmSizeMismatch { declared: u64, actual: u64 },
    /// `wasm_hash` is not the ContentHash of `wasm_bytes` (a swapped module).
    #[error("labeler wasm_hash does not match wasm_bytes")]
    WasmHashMismatch,
    /// `signature` is not [`LABELER_SIGNATURE_LEN`] bytes.
    #[error("labeler signature must be {LABELER_SIGNATURE_LEN} bytes, got {0}")]
    SignatureLen(usize),
    /// `algorithm_id` is not a valid Ed25519 verifying key.
    #[error("labeler algorithm_id is not a valid Ed25519 pubkey")]
    InvalidPubkey,
    /// Canonical re-encode of the metadata (for the signed bytes) failed.
    #[error("labeler metadata re-encode failed: {0}")]
    Encode(String),
    /// The Ed25519 signature did not verify against `algorithm_id`.
    #[error("labeler signature verification failed")]
    SignatureInvalid,
    /// The artifact's self-claimed `algorithm_id` is not the labeler the
    /// caller was asked to run ([`verify_labeler_metadata_for`]): the store
    /// answered an `inspect` for one id with another publisher's validly
    /// signed module.
    #[error("labeler algorithm_id {actual} is not the expected labeler {expected}")]
    AlgorithmIdMismatch { expected: String, actual: String },
}

/// Verify a published labeler artifact against its signed metadata — the shared
/// pre-trust check at BOTH boundaries (labeler-registry design § 6; security
/// review B1): the nest publish gate ([`crate`] consumer
/// `validate_labeler_publish`) AND every position that runs a module, which
/// MUST re-verify before **every** instantiation.
///
/// Checks, in order: (1) `wasm_size` matches the actual bytes; (2) `wasm_hash`
/// is the BLAKE3 ContentHash of the bytes (metadata↔artifact binding); (3) the
/// Ed25519 `signature` verifies against `algorithm_id` (the public-key-is-
/// identity convention) over the canonical dag-cbor of the metadata with
/// `signature` zeroed (the `GrantEvent::verify` convention). It does **not**
/// compile the module — that publish-time gate stays at the nest; the holder
/// compiles the verified bytes as part of running.
///
/// **What this does and does not enforce.** It binds the
/// bytes to the metadata and the metadata to the key the artifact *itself*
/// names — so a store cannot alter a module behind its publisher's signature.
/// It says nothing about *which* labeler the caller meant to run: a store
/// that answers an `inspect` for id A with publisher B's validly signed
/// artifact passes this check, because B's artifact is consistent with
/// itself. A position that fetched a module *by id* — the mail holder
/// draining a `labeler:<id>` factor, a subscriber's feed composing one — must
/// use [`verify_labeler_metadata_for`], which adds the expected-id check.
pub fn verify_labeler_metadata(
    metadata: &AlgorithmLabeler,
    wasm_bytes: &[u8],
) -> Result<(), LabelerVerifyError> {
    if metadata.wasm_size != wasm_bytes.len() as u64 {
        return Err(LabelerVerifyError::WasmSizeMismatch {
            declared: metadata.wasm_size,
            actual: wasm_bytes.len() as u64,
        });
    }
    if !metadata.wasm_hash.matches(wasm_bytes) {
        return Err(LabelerVerifyError::WasmHashMismatch);
    }
    if metadata.signature.len() != LABELER_SIGNATURE_LEN {
        return Err(LabelerVerifyError::SignatureLen(metadata.signature.len()));
    }
    let mut sig_bytes = [0u8; LABELER_SIGNATURE_LEN];
    sig_bytes.copy_from_slice(&metadata.signature);
    let sig = Signature::from_bytes(&sig_bytes);

    // Sign/verify over the canonical metadata with `signature` zeroed.
    let mut placeholder = metadata.clone();
    placeholder.signature = vec![0u8; LABELER_SIGNATURE_LEN];
    let bytes = crate::encoding::canonical_encode(&placeholder)
        .map_err(|e| LabelerVerifyError::Encode(e.to_string()))?;

    // Strict, via the one primitive: `algorithm_id` is the artifact's OWN
    // claimed key, so a permissive verify would let a small-order id carry an
    // all-zero signature and publish a "verified" labeler nobody holds a key to
    // (`security.md` § Key material and signature verification).
    if !crate::identity::verify_detached(&metadata.algorithm_id.0, &bytes, &sig.to_bytes()) {
        return Err(LabelerVerifyError::SignatureInvalid);
    }
    Ok(())
}

/// [`verify_labeler_metadata`] for a position that fetched the module **by
/// id** — the one shared expected-id check: after the self-consistency checks, the artifact's own
/// `algorithm_id` must equal `expected_id`, the id the caller resolved from
/// the factor it is about to score (`labeler_factor_id`), its subscription,
/// or its grant. Otherwise a store that holds any validly signed module can
/// answer an `inspect` for the labeler the user chose with a module of its
/// own choosing, and every downstream check passes.
///
/// Order matters for the error the caller sees: a tampered artifact reports
/// its tampering; only a self-consistent artifact for the *wrong* labeler
/// reports [`LabelerVerifyError::AlgorithmIdMismatch`].
pub fn verify_labeler_metadata_for(
    metadata: &AlgorithmLabeler,
    wasm_bytes: &[u8],
    expected_id: &ActorId,
) -> Result<(), LabelerVerifyError> {
    verify_labeler_metadata(metadata, wasm_bytes)?;
    if metadata.algorithm_id != *expected_id {
        return Err(LabelerVerifyError::AlgorithmIdMismatch {
            expected: expected_id.to_hex(),
            actual: metadata.algorithm_id.to_hex(),
        });
    }
    Ok(())
}

/// Sign a labeler artifact's metadata — the signer twin of
/// [`verify_labeler_metadata`], so the sign/verify convention lives in exactly
/// one place (the publisher client that produces `metadata_blob` and the
/// verifier at both boundaries must agree byte-for-byte). Stamps `signature` =
/// Ed25519 over the canonical dag-cbor of `metadata` with `signature` zeroed
/// (the `GrantEvent::verify` convention).
///
/// The caller sets `metadata.algorithm_id` = `signing_key.verifying_key()` (the
/// public-key-is-identity convention) so [`verify_labeler_metadata`] accepts the
/// result; this function only stamps the signature and does not touch
/// `algorithm_id`, `wasm_hash`, or `wasm_size` (a caller computes those). The
/// production publisher path is [`crate`]'s FFI/WASM twin
/// (`fauna_ffi::labeler::build_signed_labeler_metadata`).
pub fn sign_labeler_metadata(
    signing_key: &SigningKey,
    mut metadata: AlgorithmLabeler,
) -> Result<AlgorithmLabeler, LabelerVerifyError> {
    metadata.signature = vec![0u8; LABELER_SIGNATURE_LEN];
    let signed_over = crate::encoding::canonical_encode(&metadata)
        .map_err(|e| LabelerVerifyError::Encode(e.to_string()))?;
    metadata.signature = signing_key.sign(&signed_over).to_bytes().to_vec();
    Ok(metadata)
}

#[cfg(test)]
mod composition_tests {
    use super::*;
    use crate::encoding::{canonical_decode, canonical_encode};

    fn sample() -> Vec<CompositionEntry> {
        vec![
            CompositionEntry {
                factor: factor::ENGAGEMENT.to_string(),
                weight_permille: 1000,
            },
            CompositionEntry {
                factor: "labeler:aabb".to_string(),
                weight_permille: -1000,
            },
        ]
    }

    #[test]
    fn composition_round_trips_canonically() {
        let entries = sample();
        let bytes1 = canonical_encode(&entries).unwrap();
        let decoded: Vec<CompositionEntry> = canonical_decode(&bytes1).unwrap();
        assert_eq!(entries, decoded);
        let bytes2 = canonical_encode(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    /// Golden at-rest bytes — `feeds.composition` stores exactly this
    /// canonical dag-cbor. A change here is a storage-format break
    /// (version-compatibility.md: additive-everywhere), not a refactor.
    #[test]
    fn composition_golden_bytes() {
        let bytes = canonical_encode(&sample()).unwrap();
        assert_eq!(
            hex::encode(&bytes),
            "82a266666163746f726a656e676167656d656e746f7765696768745f7065726d\
             696c6c651903e8a266666163746f726c6c6162656c65723a616162626f776569\
             6768745f7065726d696c6c653903e7"
        );
    }

    #[test]
    fn validate_accepts_sample_and_empty() {
        assert!(validate_composition(&sample()).is_ok());
        assert!(validate_composition(&[]).is_ok());
    }

    #[test]
    fn validate_rejects_empty_factor() {
        let entries = vec![CompositionEntry {
            factor: String::new(),
            weight_permille: 1,
        }];
        assert_eq!(
            validate_composition(&entries),
            Err(CompositionError::EmptyFactor(0))
        );
    }

    #[test]
    fn validate_rejects_duplicate_factor() {
        let mut entries = sample();
        entries.push(CompositionEntry {
            factor: factor::ENGAGEMENT.to_string(),
            weight_permille: 5,
        });
        assert_eq!(
            validate_composition(&entries),
            Err(CompositionError::DuplicateFactor(
                factor::ENGAGEMENT.to_string()
            ))
        );
    }

    #[test]
    fn muted_keywords_penalty_entry_matches_and_abstains() {
        let muted = vec![MutedKeyword::from("Spoiler")];
        let entry = muted_keywords_penalty_entry(&muted, "big SPOILER inside").unwrap();
        assert_eq!(entry.factor, factor::MUTED_KEYWORDS);
        assert_eq!(entry.score, MUTED_KEYWORDS_PENALTY);
        assert_eq!(entry.tier, TIER_USER);
        assert!(muted_keywords_penalty_entry(&muted, "all clear").is_none());
        assert!(muted_keywords_penalty_entry(&[], "anything").is_none());
    }

    /// A keyword is muted at the default weight, the full penalty.
    #[test]
    fn a_keyword_mutes_at_the_full_penalty_by_default() {
        assert_eq!(MutedKeyword::from("spoiler").weight, MUTED_KEYWORDS_PENALTY);
        assert_eq!(
            MutedKeyword::new("spoiler").weight,
            MUTED_KEYWORD_DEFAULT_WEIGHT
        );
        assert_eq!(MUTED_KEYWORD_DEFAULT_WEIGHT, MUTED_KEYWORDS_PENALTY);
    }

    /// The page's two levels are two weights, and a stored weight reads back
    /// as a level by the collapse threshold — the same decision the reveal
    /// makes, so the picker never shows "Hide" over a term that does not hide.
    #[test]
    fn a_level_is_a_weight_and_a_weight_reads_back_as_the_level_that_collapses() {
        assert_eq!(MutedKeywordLevel::Hide.weight(), MUTED_KEYWORDS_PENALTY);
        assert_eq!(
            MutedKeywordLevel::ShowLess.weight(),
            MUTED_KEYWORD_SHOW_LESS_WEIGHT
        );
        for level in [MutedKeywordLevel::Hide, MutedKeywordLevel::ShowLess] {
            assert_eq!(MutedKeywordLevel::of(level.weight()), level);
        }
        assert_eq!(
            MutedKeyword::from("spoiler").level(),
            MutedKeywordLevel::Hide,
            "a new term is hidden"
        );
        for weight in [-999, -250, 0, 500] {
            let term = MutedKeyword {
                keyword: "x".into(),
                weight,
            };
            assert_eq!(term.level(), MutedKeywordLevel::ShowLess);
            assert!(!muted_keywords_collapse(std::slice::from_ref(&term), "x"));
        }
        for weight in [-1000, -5000] {
            let term = MutedKeyword {
                keyword: "x".into(),
                weight,
            };
            assert_eq!(term.level(), MutedKeywordLevel::Hide);
            assert!(muted_keywords_collapse(std::slice::from_ref(&term), "x"));
        }
        assert_eq!(
            serde_json::to_string(&MutedKeywordLevel::ShowLess).unwrap(),
            "\"show-less\"",
            "the wasm face and the ui.yaml option value spell a level the same way"
        );
    }

    /// The per-keyword weight is the factor value a match contributes; a soft
    /// weight demotes rather than sinks.
    #[test]
    fn a_soft_weight_contributes_its_own_value() {
        let muted = vec![MutedKeyword {
            keyword: "politics".into(),
            weight: -200,
        }];
        let entry = muted_keywords_penalty_entry(&muted, "more Politics today").unwrap();
        assert_eq!(entry.score, -200);
    }

    /// Several matching keywords contribute the strongest one's weight — the
    /// most negative — never their sum: the factor stays one bounded value, so
    /// a body naming three soft-muted terms is not sunk past a hard mute.
    #[test]
    fn several_matching_keywords_contribute_the_strongest_weight_not_the_sum() {
        let muted = vec![
            MutedKeyword {
                keyword: "a".into(),
                weight: -100,
            },
            MutedKeyword {
                keyword: "b".into(),
                weight: -300,
            },
            MutedKeyword {
                keyword: "zzz".into(),
                weight: MUTED_KEYWORDS_PENALTY,
            },
        ];
        let entry = muted_keywords_penalty_entry(&muted, "a and b").unwrap();
        assert_eq!(entry.score, -300);
    }

    /// The collapse — a conversation message or feed post folded behind its
    /// reveal — fires only for a keyword muted at the full penalty. A softer
    /// weight only orders ranked surfaces.
    #[test]
    fn only_a_full_weight_keyword_collapses() {
        let soft = MutedKeyword {
            keyword: "politics".into(),
            weight: -200,
        };
        let hard = MutedKeyword::from("spoiler");
        assert!(!muted_keywords_collapse(
            std::slice::from_ref(&soft),
            "politics"
        ));
        assert!(muted_keywords_collapse(
            std::slice::from_ref(&hard),
            "spoiler ahead"
        ));
        assert!(muted_keywords_collapse(&[soft, hard], "politics spoiler"));
        assert!(!muted_keywords_collapse(&[], "anything"));
    }

    /// A stored weight outside the range is read as its nearest bound, so a
    /// promotion can never ride the penalty factor and no value sinks past the
    /// full penalty.
    #[test]
    fn weights_clamp_into_the_penalty_range() {
        assert_eq!(clamp_muted_keyword_weight(500), 0);
        assert_eq!(clamp_muted_keyword_weight(-5000), MUTED_KEYWORDS_PENALTY);
        assert_eq!(clamp_muted_keyword_weight(-250), -250);
        let muted = vec![MutedKeyword {
            keyword: "x".into(),
            weight: -9000,
        }];
        assert_eq!(
            muted_keywords_penalty_entry(&muted, "x").unwrap().score,
            MUTED_KEYWORDS_PENALTY
        );
    }

    #[test]
    fn validate_rejects_too_many_entries() {
        let entries: Vec<CompositionEntry> = (0..=MAX_COMPOSITION_ENTRIES)
            .map(|i| CompositionEntry {
                factor: format!("f{i}"),
                weight_permille: 1,
            })
            .collect();
        assert_eq!(
            validate_composition(&entries),
            Err(CompositionError::TooManyEntries(
                MAX_COMPOSITION_ENTRIES + 1
            ))
        );
    }
}

#[cfg(test)]
mod score_entry_tests {
    use super::*;
    use crate::encoding::{canonical_decode, canonical_encode};

    #[test]
    fn score_entry_roundtrip() {
        let entry = ScoreEntry {
            factor: factor::RSPAMD.to_string(),
            score: -420,
            tier: TIER_ADMIN,
            scorer_version: scorer_version::RSPAMD,
        };
        let bytes = canonical_encode(&entry).unwrap();
        let decoded: ScoreEntry = canonical_decode(&bytes).unwrap();
        assert_eq!(decoded, entry);
    }

    #[test]
    fn builtin_factor_versions_covers_the_factor_namespace() {
        let list = builtin_factor_versions();
        // Every entry is a non-empty factor name at a real (>=1) source version.
        for (f, v) in list {
            assert!(!f.is_empty());
            assert!(v >= 1, "{f} seeded below v1");
        }
        // The four auth_* factors share the single AUTH source version (one
        // scorer, four factor rows) — the drain compares each per-factor.
        let auth: Vec<u32> = list
            .iter()
            .filter(|(f, _)| f.starts_with("auth_"))
            .map(|(_, v)| *v)
            .collect();
        assert_eq!(auth.len(), 4, "expected four auth_* factors");
        assert!(auth.iter().all(|&v| v == scorer_version::AUTH));
        // The tier-1 user spam factor is present at its source version.
        assert!(
            list.iter()
                .any(|&(f, v)| f == factor::SPAM && v == scorer_version::SPAM)
        );
    }

    #[test]
    fn score_entry_vec_roundtrip() {
        let entries = vec![
            ScoreEntry {
                factor: factor::SPAM.to_string(),
                score: 875,
                tier: TIER_USER,
                scorer_version: scorer_version::SPAM,
            },
            ScoreEntry {
                factor: factor::CLAMAV.to_string(),
                score: 1000,
                tier: TIER_ADMIN,
                scorer_version: scorer_version::CLAMAV,
            },
        ];
        let bytes = canonical_encode(&entries).unwrap();
        let decoded: Vec<ScoreEntry> = canonical_decode(&bytes).unwrap();
        assert_eq!(decoded, entries);
    }
}

#[cfg(test)]
mod labeler_tests {
    use super::*;
    use crate::data::ContentHash;
    use crate::encoding::{canonical_decode, canonical_encode};

    #[test]
    fn algorithm_labeler_roundtrip() {
        let labeler = AlgorithmLabeler {
            algorithm_id: crate::identity::ActorId([1u8; 32]),
            version: 1,
            wasm_hash: ContentHash::from_digest_raw([2u8; 32]),
            wasm_size: 50_000,
            input_schema: LabelerInput {
                needs_text: true,
                needs_hashtags: true,
                needs_media_metadata: false,
                needs_author: false,
                needs_attachment_bytes: false,
            },
            output_schema: LabelerOutput::default(),
            resource_limits: ScorerLimits {
                max_memory_bytes: 10_000_000,
                max_cpu_microseconds: 100_000,
            },
            updated_at: crate::data::Timestamp(1000000),
            signature: vec![0xAB; 64],
        };
        let bytes = canonical_encode(&labeler).unwrap();
        let decoded: AlgorithmLabeler = canonical_decode(&bytes).unwrap();
        assert_eq!(decoded.algorithm_id, labeler.algorithm_id);
        assert_eq!(decoded.version, labeler.version);
        assert_eq!(decoded.wasm_hash, labeler.wasm_hash);
        assert!(decoded.input_schema.needs_text);
        assert!(!decoded.input_schema.needs_media_metadata);
    }

    /// Build a validly-signed labeler artifact over `wasm_bytes` (the fixture the
    /// shared verify + the FFI holder consume). Deterministic — a fixed seed key.
    fn signed_labeler(wasm_bytes: &[u8]) -> AlgorithmLabeler {
        use ed25519_dalek::SigningKey;
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let meta = AlgorithmLabeler {
            algorithm_id: crate::identity::ActorId(sk.verifying_key().to_bytes()),
            version: 1,
            wasm_hash: crate::encoding::content_hash(wasm_bytes),
            wasm_size: wasm_bytes.len() as u64,
            input_schema: LabelerInput {
                needs_text: true,
                needs_hashtags: false,
                needs_media_metadata: false,
                needs_author: false,
                needs_attachment_bytes: false,
            },
            output_schema: LabelerOutput::default(),
            resource_limits: ScorerLimits {
                max_memory_bytes: 10_000_000,
                max_cpu_microseconds: 100_000,
            },
            updated_at: crate::data::Timestamp(1_000_000),
            signature: vec![0u8; LABELER_SIGNATURE_LEN],
        };
        // The sign convention lives in one place — `signed_labeler` is just a
        // deterministic fixture over it (its round-trip vs. `verify` below pins
        // the sign↔verify agreement).
        sign_labeler_metadata(&sk, meta).unwrap()
    }

    #[test]
    fn verify_labeler_metadata_accepts_valid_and_rejects_tampering() {
        let wasm = b"\0asm-ish-module-bytes-for-the-verify-fixture".to_vec();
        let meta = signed_labeler(&wasm);
        // Valid artifact verifies.
        assert_eq!(verify_labeler_metadata(&meta, &wasm), Ok(()));

        // A swapped module (same length, one byte flipped) fails the hash bind —
        // this is exactly the silent-store-swap B1 defends against.
        let mut swapped = wasm.clone();
        swapped[0] ^= 0xFF;
        assert_eq!(
            verify_labeler_metadata(&meta, &swapped),
            Err(LabelerVerifyError::WasmHashMismatch)
        );

        // A wrong declared size fails before the hash check.
        let mut bad_size = meta.clone();
        bad_size.wasm_size += 1;
        assert!(matches!(
            verify_labeler_metadata(&bad_size, &wasm),
            Err(LabelerVerifyError::WasmSizeMismatch { .. })
        ));

        // A forged signature (valid length, wrong bytes) fails the Ed25519 check.
        let mut bad_sig = meta.clone();
        bad_sig.signature = vec![0xABu8; LABELER_SIGNATURE_LEN];
        assert_eq!(
            verify_labeler_metadata(&bad_sig, &wasm),
            Err(LabelerVerifyError::SignatureInvalid)
        );

        // A wrong-length signature is rejected structurally.
        let mut short_sig = meta.clone();
        short_sig.signature = vec![0u8; 10];
        assert_eq!(
            verify_labeler_metadata(&short_sig, &wasm),
            Err(LabelerVerifyError::SignatureLen(10))
        );
    }

    /// A validly signed module that is not the labeler the caller asked for
    /// is refused by the expected-id form and accepted by the bare form —
    /// the difference is the whole finding: a store answering
    /// `inspect(A)` with publisher B's self-consistent artifact.
    #[test]
    fn verify_labeler_metadata_for_pins_the_expected_id() {
        let wasm = b"\0asm-ish-module-bytes-for-the-verify-fixture".to_vec();
        let meta = signed_labeler(&wasm);
        let own_id = meta.algorithm_id;
        assert_eq!(verify_labeler_metadata_for(&meta, &wasm, &own_id), Ok(()));

        let other = crate::identity::ActorId([0x42u8; 32]);
        assert_eq!(verify_labeler_metadata(&meta, &wasm), Ok(()));
        assert_eq!(
            verify_labeler_metadata_for(&meta, &wasm, &other),
            Err(LabelerVerifyError::AlgorithmIdMismatch {
                expected: other.to_hex(),
                actual: own_id.to_hex(),
            })
        );

        // Tampering is reported as tampering even when the id also differs:
        // the self-consistency checks run first.
        let mut swapped = wasm.clone();
        swapped[0] ^= 0xFF;
        assert_eq!(
            verify_labeler_metadata_for(&meta, &swapped, &other),
            Err(LabelerVerifyError::WasmHashMismatch)
        );
    }

    #[test]
    fn sign_labeler_metadata_is_the_verify_twin() {
        use ed25519_dalek::SigningKey;
        // `sign_labeler_metadata` stamps a signature `verify_labeler_metadata`
        // accepts, over the same canonical-with-signature-zeroed convention —
        // the whole point of colocating the two so the publisher (client/FFI)
        // and both verify boundaries agree byte-for-byte.
        let wasm = b"\0asm-twin-fixture".to_vec();
        let sk = SigningKey::from_bytes(&[42u8; 32]);
        let unsigned = AlgorithmLabeler {
            algorithm_id: crate::identity::ActorId(sk.verifying_key().to_bytes()),
            version: 3,
            wasm_hash: crate::encoding::content_hash(&wasm),
            wasm_size: wasm.len() as u64,
            input_schema: LabelerInput {
                needs_text: true,
                needs_hashtags: false,
                needs_media_metadata: false,
                needs_author: false,
                needs_attachment_bytes: false,
            },
            output_schema: LabelerOutput::default(),
            resource_limits: ScorerLimits {
                max_memory_bytes: 10_000_000,
                max_cpu_microseconds: 100_000,
            },
            updated_at: crate::data::Timestamp(7),
            signature: vec![], // any prior value is overwritten by the sign
        };
        let signed = sign_labeler_metadata(&sk, unsigned).unwrap();
        assert_eq!(signed.signature.len(), LABELER_SIGNATURE_LEN);
        assert_eq!(verify_labeler_metadata(&signed, &wasm), Ok(()));

        // Signing under a different key yields an id/signature pair `verify`
        // rejects (the signature is over the wrong algorithm_id's convention).
        let other = SigningKey::from_bytes(&[43u8; 32]);
        let mut mismatched = signed.clone();
        mismatched.signature = sign_labeler_metadata(&other, signed).unwrap().signature;
        assert_eq!(
            verify_labeler_metadata(&mismatched, &wasm),
            Err(LabelerVerifyError::SignatureInvalid)
        );
    }

    #[test]
    fn labeler_factor_is_prefixed_and_never_a_builtin() {
        use crate::identity::ActorId;

        let f = labeler_factor(&ActorId([0xAB; 32]));
        // Prefixed with the reserved namespace + lowercase-hex of the id.
        assert!(f.starts_with("labeler:"), "{f} must start with 'labeler:'");
        assert_eq!(f, format!("labeler:{}", "ab".repeat(32)));
        // Never collides with any built-in bare-word factor.
        for (builtin, _) in builtin_factor_versions() {
            assert_ne!(f, builtin, "labeler factor collided with builtin {builtin}");
            assert!(
                !builtin.contains(':'),
                "builtin {builtin} unexpectedly carries the labeler ':' separator"
            );
        }
        // Distinct ids yield distinct factors.
        assert_ne!(
            labeler_factor(&ActorId([1u8; 32])),
            labeler_factor(&ActorId([2u8; 32]))
        );
    }

    fn lbl(category: &str, confidence: f64) -> Label {
        Label {
            category: category.to_string(),
            confidence,
            source: LabelSource::TextAnalysis,
        }
    }

    #[test]
    fn labels_to_score_entry_maps_primary_confidence_to_per_mille() {
        let factor = labeler_factor(&crate::identity::ActorId([0xCA; 32]));

        // Empty output (no detection) → score 0, still stamped tier-3 at version.
        let empty = labels_to_score_entry(&[], factor.clone(), 7);
        assert_eq!(empty.score, 0);
        assert_eq!(empty.factor, factor);
        assert_eq!(empty.tier, TIER_COMMUNITY);
        assert_eq!(empty.scorer_version, 7);

        // Single label: confidence 0.5 → 500 per-mille.
        let one = labels_to_score_entry(&[lbl("cat", 0.5)], factor.clone(), 7);
        assert_eq!(one.score, 500);

        // Multi-label: the highest-confidence label is the primary (0.87 → 870).
        let many = labels_to_score_entry(
            &[lbl("dog", 0.30), lbl("cat", 0.87), lbl("bird", 0.10)],
            factor.clone(),
            7,
        );
        assert_eq!(many.score, 870);

        // Full confidence → 1000; rounds half away from zero (0.9995 → 1000).
        assert_eq!(
            labels_to_score_entry(&[lbl("x", 1.0)], factor.clone(), 7).score,
            1000
        );
        assert_eq!(
            labels_to_score_entry(&[lbl("x", 0.9995)], factor.clone(), 7).score,
            1000
        );

        // an untrusted module's out-of-range / NaN
        // confidence is clamped into [0,1] so the per-mille score stays in
        // [0,1000] (never a giant/negative value a ranking consumer would trip on).
        assert_eq!(
            labels_to_score_entry(&[lbl("x", 5.0)], factor.clone(), 7).score,
            1000,
            "over-range confidence clamps to 1000"
        );
        assert_eq!(
            labels_to_score_entry(&[lbl("x", -3.0)], factor.clone(), 7).score,
            0,
            "negative confidence clamps to 0"
        );
        assert_eq!(
            labels_to_score_entry(&[lbl("x", f64::NAN)], factor.clone(), 7).score,
            0,
            "NaN confidence is dropped by fold → score 0"
        );
        // A NaN alongside a real label still yields the real label's score.
        assert_eq!(
            labels_to_score_entry(&[lbl("x", f64::NAN), lbl("y", 0.4)], factor, 7).score,
            400,
            "a real label wins over a NaN sibling"
        );
    }

    fn list_bytes(entries: Vec<(u8, i64)>) -> Vec<u8> {
        // Build an artifact whose ids are `[b; 32]` — ascending iff `b` ascends.
        let artifact = LabelerListArtifact {
            entries: entries
                .into_iter()
                .map(|(b, score)| ListEntry {
                    content_id: serde_bytes::ByteBuf::from(vec![b; 32]),
                    score,
                })
                .collect(),
            name: None,
        };
        crate::encoding::canonical_encode(&artifact).unwrap()
    }

    /// The pre-name `LabelerListArtifact` shape, verbatim — what an **older
    /// nest** decodes a published artifact into. Its acceptance of a *named*
    /// artifact's bytes is the bidirectional-compat proof: within a major
    /// version a client may be newer than the nest it talks to, so evolution is
    /// additive-everywhere (`docs/goal/architecture/version-compatibility.md`).
    #[derive(Serialize, Deserialize)]
    struct OldShapeListArtifact {
        entries: Vec<ListEntry>,
    }

    #[test]
    fn unnamed_list_artifact_encodes_byte_identically_to_the_pre_name_shape() {
        // The `#[serde(skip_serializing_if)]` pin: adding the name axis must not
        // move a single byte for the artifacts that predate it, or every stored
        // signature over `wasm_hash` would break. Mirrors the `TopicModel`
        // engagement-counter `golden_bytes` pin (topic-factors.md § v2).
        let old = OldShapeListArtifact {
            entries: vec![ListEntry {
                content_id: serde_bytes::ByteBuf::from(vec![7u8; 32]),
                score: 640,
            }],
        };
        let old_bytes = crate::encoding::canonical_encode(&old).unwrap();
        assert_eq!(
            list_bytes(vec![(7, 640)]),
            old_bytes,
            "an unnamed list must serialize byte-identically to the pre-name shape"
        );
    }

    #[test]
    fn an_older_nest_accepts_a_named_artifact_and_ignores_the_name() {
        // THE compat proof for riding the name inside the artifact: the name is
        // an unknown field to a nest that predates it, and serde ignores unknown
        // fields (no `deny_unknown_fields`), so the older nest still validates
        // and materializes the entries. `wasm_hash` binds the raw bytes with no
        // re-encode, so the publisher's signature verifies there unchanged.
        let named = build_list_artifact(Some("Small orange cats"), vec![([9u8; 32], 900)]).unwrap();

        let seen: OldShapeListArtifact =
            crate::encoding::canonical_decode(&named).expect("an older nest decodes a named list");
        assert_eq!(seen.entries.len(), 1);
        assert_eq!(seen.entries[0].score, 900);
        assert_eq!(seen.entries[0].content_id.as_ref(), &[9u8; 32]);
    }

    #[test]
    fn validate_list_artifact_round_trips_the_publisher_chosen_name() {
        let bytes = build_list_artifact(Some("Small orange cats"), vec![([1u8; 32], 10)]).unwrap();
        let artifact = validate_list_artifact(&bytes).unwrap();
        assert_eq!(artifact.name.as_deref(), Some("Small orange cats"));
        // An unnamed list stays valid — the name axis is optional.
        assert_eq!(
            validate_list_artifact(&list_bytes(vec![(1, 10)]))
                .unwrap()
                .name,
            None
        );
    }

    #[test]
    fn validate_list_artifact_rejects_an_over_long_or_blank_name() {
        let long = "c".repeat(MAX_LABELER_LIST_NAME_LEN + 1);
        let artifact = LabelerListArtifact {
            entries: vec![],
            name: Some(long.clone()),
        };
        let bytes = crate::encoding::canonical_encode(&artifact).unwrap();
        assert!(matches!(
            validate_list_artifact(&bytes),
            Err(ListArtifactError::NameTooLong(n)) if n == long.chars().count()
        ));

        let blank = LabelerListArtifact {
            entries: vec![],
            name: Some("   ".into()),
        };
        let bytes = crate::encoding::canonical_encode(&blank).unwrap();
        assert!(matches!(
            validate_list_artifact(&bytes),
            Err(ListArtifactError::BlankName)
        ));
    }

    #[test]
    fn build_list_artifact_sorts_dedups_and_produces_canonical_bytes() {
        // The builder is what every app publishes through, so it — not the
        // caller — owes the canonical form the nest gate demands.
        let bytes = build_list_artifact(
            None,
            vec![([3u8; 32], 300), ([1u8; 32], 100), ([2u8; 32], 200)],
        )
        .unwrap();
        let artifact = validate_list_artifact(&bytes).expect("builder output is canonical");
        let ids: Vec<u8> = artifact.entries.iter().map(|e| e.content_id[0]).collect();
        assert_eq!(ids, vec![1, 2, 3], "entries come back strictly ascending");

        // Last-wins on a duplicate id (the pruning UI can hand the same id twice).
        let bytes = build_list_artifact(None, vec![([5u8; 32], 100), ([5u8; 32], 900)]).unwrap();
        let artifact = validate_list_artifact(&bytes).unwrap();
        assert_eq!(artifact.entries.len(), 1, "duplicates collapse");
        assert_eq!(artifact.entries[0].score, 900, "the later entry wins");
    }

    #[test]
    fn build_list_artifact_rejects_out_of_contract_input() {
        assert!(matches!(
            build_list_artifact(None, vec![([1u8; 32], 1001)]),
            Err(ListArtifactError::ScoreOutOfRange { .. })
        ));
        assert!(matches!(
            build_list_artifact(Some("  "), vec![]),
            Err(ListArtifactError::BlankName)
        ));
    }

    #[test]
    fn validate_list_artifact_accepts_canonical_and_round_trips() {
        let bytes = list_bytes(vec![(1, 0), (2, 500), (3, 1000)]);
        let artifact = validate_list_artifact(&bytes).unwrap();
        assert_eq!(artifact.entries.len(), 3);
        assert_eq!(artifact.entries[1].score, 500);
        assert_eq!(artifact.entries[1].content_id.as_ref(), &[2u8; 32]);
        // The empty list is valid (a curator may publish-then-fill).
        assert_eq!(
            validate_list_artifact(&list_bytes(vec![]))
                .unwrap()
                .entries
                .len(),
            0
        );
    }

    #[test]
    fn validate_list_artifact_rejects_noncanonical_and_out_of_contract() {
        // Not dag-cbor / wrong shape.
        assert!(matches!(
            validate_list_artifact(b"\0asm not cbor"),
            Err(ListArtifactError::Decode(_))
        ));
        // Unsorted.
        assert!(matches!(
            validate_list_artifact(&list_bytes(vec![(2, 10), (1, 10)])),
            Err(ListArtifactError::NotStrictlyAscending(1))
        ));
        // Duplicate id.
        assert!(matches!(
            validate_list_artifact(&list_bytes(vec![(1, 10), (1, 20)])),
            Err(ListArtifactError::NotStrictlyAscending(1))
        ));
        // Score out of range — both sides.
        assert!(matches!(
            validate_list_artifact(&list_bytes(vec![(1, 1001)])),
            Err(ListArtifactError::ScoreOutOfRange {
                index: 0,
                score: 1001
            })
        ));
        assert!(matches!(
            validate_list_artifact(&list_bytes(vec![(1, -1)])),
            Err(ListArtifactError::ScoreOutOfRange {
                index: 0,
                score: -1
            })
        ));
        // Wrong id length.
        let bad_id = LabelerListArtifact {
            entries: vec![ListEntry {
                content_id: serde_bytes::ByteBuf::from(vec![7u8; 16]),
                score: 5,
            }],
            name: None,
        };
        let bytes = crate::encoding::canonical_encode(&bad_id).unwrap();
        assert!(matches!(
            validate_list_artifact(&bytes),
            Err(ListArtifactError::BadContentIdLen { index: 0, len: 16 })
        ));
        // Entry-count cap.
        let over = LabelerListArtifact {
            entries: (0..=MAX_LABELER_LIST_ENTRIES)
                .map(|i| {
                    let mut id = [0u8; 32];
                    id[..8].copy_from_slice(&(i as u64).to_be_bytes());
                    ListEntry {
                        content_id: serde_bytes::ByteBuf::from(id.to_vec()),
                        score: 0,
                    }
                })
                .collect(),
            name: None,
        };
        let bytes = crate::encoding::canonical_encode(&over).unwrap();
        assert!(matches!(
            validate_list_artifact(&bytes),
            Err(ListArtifactError::TooManyEntries(_))
        ));
    }

    // ── the attachment-bytes flag's compat pins ─────────────────────────
    //
    // `LabelerInput.needs_attachment_bytes` is additive on a SIGNED record
    // whose signature covers the canonical re-encode, so the two directions of
    // `version-compatibility.md` § I4 are pinned here against verbatim copies
    // of the pre-flag shapes — what a holder that predates the flag decodes.

    /// `LabelerInput` as every holder before the flag had it, verbatim.
    #[derive(Debug, Clone, Copy, Serialize, Deserialize)]
    struct PreFlagLabelerInput {
        needs_text: bool,
        needs_hashtags: bool,
        needs_media_metadata: bool,
        needs_author: bool,
    }

    /// `AlgorithmLabeler` over the pre-flag input schema, verbatim.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct PreFlagAlgorithmLabeler {
        algorithm_id: crate::identity::ActorId,
        version: u64,
        wasm_hash: ContentHash,
        wasm_size: u64,
        input_schema: PreFlagLabelerInput,
        resource_limits: ScorerLimits,
        updated_at: crate::data::Timestamp,
        #[serde(with = "serde_bytes")]
        signature: Vec<u8>,
    }

    fn flagged_signed_labeler(wasm_bytes: &[u8], needs_attachment_bytes: bool) -> AlgorithmLabeler {
        use ed25519_dalek::SigningKey;
        let sk = SigningKey::from_bytes(&[9u8; 32]);
        let meta = AlgorithmLabeler {
            algorithm_id: crate::identity::ActorId(sk.verifying_key().to_bytes()),
            version: 3,
            wasm_hash: crate::encoding::content_hash(wasm_bytes),
            wasm_size: wasm_bytes.len() as u64,
            input_schema: LabelerInput {
                needs_text: false,
                needs_hashtags: false,
                needs_media_metadata: true,
                needs_author: false,
                needs_attachment_bytes,
            },
            output_schema: LabelerOutput::default(),
            resource_limits: ScorerLimits {
                max_memory_bytes: 32 * 1024 * 1024,
                max_cpu_microseconds: 250_000,
            },
            updated_at: crate::data::Timestamp(11),
            signature: Vec::new(),
        };
        sign_labeler_metadata(&sk, meta).unwrap()
    }

    #[test]
    fn a_flagless_input_schema_encodes_byte_identically_to_the_pre_flag_shape() {
        // The `skip_serializing_if` pin: an artifact that does not ask for bytes
        // must not move a single byte, or every signature stored before the
        // flag would stop verifying on a holder that knows it.
        let wasm = b"(module)";
        let signed = flagged_signed_labeler(wasm, false);
        let pre_flag = PreFlagAlgorithmLabeler {
            algorithm_id: signed.algorithm_id,
            version: signed.version,
            wasm_hash: signed.wasm_hash,
            wasm_size: signed.wasm_size,
            input_schema: PreFlagLabelerInput {
                needs_text: signed.input_schema.needs_text,
                needs_hashtags: signed.input_schema.needs_hashtags,
                needs_media_metadata: signed.input_schema.needs_media_metadata,
                needs_author: signed.input_schema.needs_author,
            },
            resource_limits: signed.resource_limits,
            updated_at: signed.updated_at,
            signature: signed.signature.clone(),
        };
        assert_eq!(
            canonical_encode(&signed).unwrap(),
            canonical_encode(&pre_flag).unwrap(),
            "a flagless schema must serialize byte-identically to the pre-flag shape"
        );
    }

    #[test]
    fn a_newer_holder_verifies_a_pre_flag_artifact_unchanged_and_reads_it_as_not_asking() {
        // The other direction: a signature made before the flag existed still
        // verifies on a holder that knows it, and decodes as "does not ask".
        use ed25519_dalek::SigningKey;
        let wasm = b"(module)";
        let sk = SigningKey::from_bytes(&[10u8; 32]);
        let mut old = PreFlagAlgorithmLabeler {
            algorithm_id: crate::identity::ActorId(sk.verifying_key().to_bytes()),
            version: 1,
            wasm_hash: crate::encoding::content_hash(wasm),
            wasm_size: wasm.len() as u64,
            input_schema: PreFlagLabelerInput {
                needs_text: true,
                needs_hashtags: false,
                needs_media_metadata: false,
                needs_author: true,
            },
            resource_limits: ScorerLimits {
                max_memory_bytes: 1 << 20,
                max_cpu_microseconds: 1_000,
            },
            updated_at: crate::data::Timestamp(5),
            signature: vec![0u8; LABELER_SIGNATURE_LEN],
        };
        // Sign exactly as a pre-flag publisher did: over the canonical bytes
        // with `signature` zeroed.
        let to_sign = canonical_encode(&old).unwrap();
        old.signature = sk.sign(&to_sign).to_bytes().to_vec();
        let bytes = canonical_encode(&old).unwrap();

        let seen: AlgorithmLabeler = canonical_decode(&bytes).expect("a newer holder decodes");
        assert!(!seen.input_schema.needs_attachment_bytes);
        assert!(seen.input_schema.needs_author);
        assert_eq!(verify_labeler_metadata(&seen, wasm), Ok(()));
    }

    #[test]
    fn an_older_holder_refuses_a_bytes_asking_artifact_rather_than_running_it_without() {
        // A holder that predates the flag decodes a flagged artifact (unknown
        // fields are ignored) but its canonical re-encode lacks the flag, so
        // the signature does not verify there: the artifact is REFUSED — at
        // publish, or at inspect as unverified — never run without the bytes it
        // declared it reads. Refusal is the posture; this pins that it is what
        // an old holder actually does, not a hope.
        let wasm = b"(module)";
        let signed = flagged_signed_labeler(wasm, true);
        assert_eq!(
            verify_labeler_metadata(&signed, wasm),
            Ok(()),
            "a holder that knows the flag verifies the flagged artifact"
        );
        let bytes = canonical_encode(&signed).unwrap();
        let seen: PreFlagAlgorithmLabeler =
            canonical_decode(&bytes).expect("an older holder decodes, ignoring the flag");
        let mut placeholder = seen.clone();
        placeholder.signature = vec![0u8; LABELER_SIGNATURE_LEN];
        let re_encoded = canonical_encode(&placeholder).unwrap();
        assert!(
            !crate::identity::verify_detached(&seen.algorithm_id.0, &re_encoded, &seen.signature),
            "the older holder's re-encode dropped the flag, so its verify must fail"
        );
    }

    // ── the output label-ABI stamp's compat pins ────────────────────────
    //
    // `AlgorithmLabeler.output_schema` is additive on the same signed record,
    // so the same two directions are pinned against a verbatim copy of the
    // shape every holder before the stamp had: all five input flags, no
    // output declaration.

    /// `AlgorithmLabeler` as every holder before the output stamp had it.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct PreStampAlgorithmLabeler {
        algorithm_id: crate::identity::ActorId,
        version: u64,
        wasm_hash: ContentHash,
        wasm_size: u64,
        input_schema: LabelerInput,
        resource_limits: ScorerLimits,
        updated_at: crate::data::Timestamp,
        #[serde(with = "serde_bytes")]
        signature: Vec<u8>,
    }

    fn stamped_signed_labeler(wasm_bytes: &[u8], label_abi: u16) -> AlgorithmLabeler {
        use ed25519_dalek::SigningKey;
        let sk = SigningKey::from_bytes(&[12u8; 32]);
        let meta = AlgorithmLabeler {
            algorithm_id: crate::identity::ActorId(sk.verifying_key().to_bytes()),
            output_schema: LabelerOutput { label_abi },
            ..flagged_signed_labeler(wasm_bytes, true)
        };
        sign_labeler_metadata(&sk, meta).unwrap()
    }

    #[test]
    fn a_revision_one_output_schema_encodes_byte_identically_to_the_pre_stamp_shape() {
        // Revision 1 is omitted: an artifact emitting the v1 `Label` record
        // must not move a byte, or every signature stored before the stamp
        // would stop verifying on a holder that knows it.
        let wasm = b"(module)";
        let signed = stamped_signed_labeler(wasm, 1);
        assert_eq!(verify_labeler_metadata(&signed, wasm), Ok(()));
        let pre_stamp = PreStampAlgorithmLabeler {
            algorithm_id: signed.algorithm_id,
            version: signed.version,
            wasm_hash: signed.wasm_hash,
            wasm_size: signed.wasm_size,
            input_schema: signed.input_schema,
            resource_limits: signed.resource_limits,
            updated_at: signed.updated_at,
            signature: signed.signature.clone(),
        };
        assert_eq!(
            canonical_encode(&signed).unwrap(),
            canonical_encode(&pre_stamp).unwrap(),
            "a revision-1 output schema must serialize byte-identically to the pre-stamp shape"
        );
        // And the other direction: the pre-stamp bytes read back as revision 1
        // on a holder that knows the stamp, and still verify there.
        let seen: AlgorithmLabeler = canonical_decode(&canonical_encode(&pre_stamp).unwrap())
            .expect("a newer holder decodes a pre-stamp artifact");
        assert_eq!(seen.output_schema, LabelerOutput { label_abi: 1 });
        assert_eq!(verify_labeler_metadata(&seen, wasm), Ok(()));
    }

    #[test]
    fn a_pre_stamp_holder_refuses_a_newer_revision_artifact_as_unverified() {
        // A holder that predates the stamp decodes a revision-2 artifact
        // (unknown fields are ignored) but its re-encode lacks the stamp, so
        // the signature does not verify there: it is refused, never run and
        // mis-decoded as a broken module.
        let wasm = b"(module)";
        let signed = stamped_signed_labeler(wasm, 2);
        assert_eq!(
            verify_labeler_metadata(&signed, wasm),
            Ok(()),
            "a holder that knows the stamp verifies the revision-2 artifact"
        );
        let bytes = canonical_encode(&signed).unwrap();
        let round: AlgorithmLabeler = canonical_decode(&bytes).unwrap();
        assert_eq!(
            round.output_schema.label_abi, 2,
            "the stamp survives the wire"
        );
        let seen: PreStampAlgorithmLabeler =
            canonical_decode(&bytes).expect("a pre-stamp holder decodes, ignoring the stamp");
        let mut placeholder = seen.clone();
        placeholder.signature = vec![0u8; LABELER_SIGNATURE_LEN];
        let re_encoded = canonical_encode(&placeholder).unwrap();
        assert!(
            !crate::identity::verify_detached(&seen.algorithm_id.0, &re_encoded, &seen.signature),
            "the pre-stamp holder's re-encode dropped the stamp, so its verify must fail"
        );
    }
}

#[cfg(test)]
mod perimeter_mail_score_rows_tests {
    use super::*;
    use crate::mail_auth::DmarcPolicy;

    #[test]
    fn maps_every_scored_factor_to_a_bus_row() {
        let rows = perimeter_mail_score_rows(
            1875,
            &ClamavVerdict::Infected {
                signature: "Eicar-Test-Signature".into(),
            },
            Some(RspamdScore {
                raw_milli: 2400,
                scaled_milli: 1200,
                flagged_rules: vec!["URIBL_BLACK".into()],
                breakdown: vec![],
            }),
            &AuthVerdicts {
                spf: SpfVerdict::SoftFail,
                dkim: DkimVerdict::Pass,
                dmarc: DmarcVerdict::Fail {
                    policy: DmarcPolicy::Quarantine,
                },
                arc: ArcVerdict::None,
            },
        );
        let get = |name: &str| {
            rows.iter()
                .find(|e| e.factor == name)
                .unwrap_or_else(|| panic!("missing factor {name}"))
        };
        assert_eq!(rows.len(), 6, "arc None emits no row: {rows:?}");
        let spam = get(factor::SPAM);
        // Milli-points, as the caller passed them — never floored to points.
        assert_eq!((spam.score, spam.tier), (1875, TIER_USER));
        assert_eq!(spam.scorer_version, scorer_version::SPAM);
        assert_eq!(
            (get(factor::CLAMAV).score, get(factor::CLAMAV).tier),
            (1000, TIER_ADMIN)
        );
        assert_eq!(get(factor::RSPAMD).score, 1200);
        assert_eq!(get(factor::AUTH_SPF).score, 500);
        assert_eq!(get(factor::AUTH_DKIM).score, 0);
        assert_eq!(get(factor::AUTH_DMARC).score, 1000);
        assert!(!rows.iter().any(|e| e.factor == factor::AUTH_ARC));
        // Every built-in row is stamped at the registry's current version, so
        // a fresh ingest never starts out owing a re-score.
        for row in &rows {
            let (_, v) = builtin_factor_versions()
                .into_iter()
                .find(|(f, _)| *f == row.factor)
                .unwrap_or_else(|| panic!("{} is not a built-in factor", row.factor));
            assert_eq!(row.scorer_version, v, "{}", row.factor);
        }
    }

    #[test]
    fn skips_factors_that_did_not_score() {
        // rspamd disabled + ClamAV oversize-bypass + all-indeterminate auth →
        // only the spam row (the perimeter spam gate always runs for
        // deliveries; 0 = "scored ham").
        let rows = perimeter_mail_score_rows(
            0,
            &ClamavVerdict::BypassedOversize,
            None,
            &AuthVerdicts::default(),
        );
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].factor, factor::SPAM);
        assert_eq!(rows[0].score, 0);
        // A scanner error is forensic, not a verdict.
        let rows = perimeter_mail_score_rows(
            0,
            &ClamavVerdict::Error {
                detail: "clamd down".into(),
            },
            None,
            &AuthVerdicts::default(),
        );
        assert!(!rows.iter().any(|e| e.factor == factor::CLAMAV), "{rows:?}");
    }
}

#[cfg(test)]
mod topic_factor_tests {
    use super::*;
    use crate::identity::ActorId;

    #[test]
    fn topic_factor_is_prefixed_and_never_a_builtin() {
        // Mirrors labeler_factor_is_prefixed_and_never_a_builtin for the
        // second namespaced factor family.
        let f = topic_factor(&[0xAB; 16]);
        assert!(f.starts_with("topic:"), "{f} must start with 'topic:'");
        assert_eq!(f, format!("topic:{}", "ab".repeat(16)));
        assert!(is_topic_factor(&f));
        // Never collides with any built-in bare-word factor (built-ins carry
        // no ':'), and the namespace check rejects every one of them.
        for (builtin, _) in builtin_factor_versions() {
            assert_ne!(f, builtin, "topic factor collided with builtin {builtin}");
            assert!(!is_topic_factor(builtin), "{builtin} is not a topic factor");
        }
        // The other namespaced families are disjoint.
        assert!(!is_topic_factor(&labeler_factor(&ActorId([0xAB; 32]))));
        assert!(!is_topic_factor(factor::REPORT_SPAM));
        assert!(!is_topic_factor(factor::SIGNAL_WATCH_COMPLETE));
        assert!(!is_topic_factor(factor::SIGNAL_SKIP));
        assert!(!is_topic_factor(factor::MUTED_KEYWORDS));
        assert!(!is_topic_factor(factor::ENGAGEMENT));
        // Distinct ids yield distinct factors.
        assert_ne!(topic_factor(&[1u8; 16]), topic_factor(&[2u8; 16]));
    }

    #[test]
    fn personalization_model_namespace_accepts_topic_and_cues_only() {
        // The sealed personalization-model wire accepts the `topic:` trained
        // model namespace AND the `cues:` engagement-cue rollup namespace
        // (engagement-cues.md § Seal + home — the additive prefix-set extension).
        assert!(is_personalization_model_factor(&topic_factor(&[0xAB; 16])));
        assert!(is_personalization_model_factor("cues:v1"));
        // A future `cues:v2` still parses by prefix (envelope-only validation).
        assert!(is_personalization_model_factor("cues:v2"));
        // But `cues:` is NOT a composition factor: `is_topic_factor` (what
        // FeedManager folds) must never claim it.
        assert!(!is_topic_factor("cues:v1"));
        // Every other namespace is rejected — no built-in, labeler, report, or
        // muted-keyword key may ride the sealed model wire.
        for (builtin, _) in builtin_factor_versions() {
            assert!(
                !is_personalization_model_factor(builtin),
                "{builtin} is not a personalization-model key"
            );
        }
        assert!(!is_personalization_model_factor(&labeler_factor(&ActorId(
            [0x11; 32]
        ))));
        assert!(!is_personalization_model_factor(factor::REPORT_SPAM));
        assert!(!is_personalization_model_factor(
            factor::SIGNAL_WATCH_COMPLETE
        ));
        assert!(!is_personalization_model_factor(factor::SIGNAL_SKIP));
        assert!(!is_personalization_model_factor(factor::MUTED_KEYWORDS));
        assert!(!is_personalization_model_factor(factor::TRENDING));
        assert!(!is_personalization_model_factor("bogus:x"));
    }

    #[test]
    fn topic_factor_id_round_trips() {
        let id: [u8; 16] = [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD,
            0xEE, 0xFF,
        ];
        assert_eq!(topic_factor_id(&topic_factor(&id)), Some(id));
        assert_eq!(topic_factor_id(&topic_factor(&[0u8; 16])), Some([0u8; 16]));
    }

    #[test]
    fn topic_factor_id_is_strict() {
        let good_hex = "ab".repeat(16);
        assert_eq!(
            topic_factor_id(&format!("topic:{good_hex}")),
            Some([0xAB; 16])
        );
        // Missing / wrong / case-variant prefix.
        assert_eq!(topic_factor_id(&good_hex), None);
        assert_eq!(topic_factor_id(&format!("TOPIC:{good_hex}")), None);
        assert_eq!(topic_factor_id(&format!("labeler:{good_hex}")), None);
        // Wrong hex length (15 and 17 bytes, and empty).
        assert_eq!(topic_factor_id(&format!("topic:{}", "ab".repeat(15))), None);
        assert_eq!(topic_factor_id(&format!("topic:{}", "ab".repeat(17))), None);
        assert_eq!(topic_factor_id("topic:"), None);
        // Uppercase hex is rejected (canonical keys are lowercase-only).
        assert_eq!(topic_factor_id(&format!("topic:{}", "AB".repeat(16))), None);
        // Non-hex digits rejected.
        assert_eq!(topic_factor_id(&format!("topic:{}", "zz".repeat(16))), None);
        // No trimming leniency.
        assert_eq!(topic_factor_id(&format!(" topic:{good_hex}")), None);
        assert_eq!(topic_factor_id(&format!("topic:{good_hex} ")), None);
        assert_eq!(topic_factor_id(&format!("topic:{good_hex}\n")), None);
        // Bare built-ins never parse.
        assert_eq!(topic_factor_id(factor::SPAM), None);
    }

    #[test]
    fn validate_composition_accepts_topic_keys() {
        // Compositions treat factor keys as opaque (the namespace is
        // open-ended by design) — pin that a `topic:` entry passes the shared
        // create/update gate alongside a built-in.
        let entries = vec![
            CompositionEntry {
                factor: factor::ENGAGEMENT.to_string(),
                weight_permille: 1000,
            },
            CompositionEntry {
                factor: topic_factor(&[0xC4; 16]),
                weight_permille: 2500,
            },
        ];
        assert_eq!(validate_composition(&entries), Ok(()));
        // The duplicate-factor rule applies to topic keys like any other.
        let dup = vec![
            CompositionEntry {
                factor: topic_factor(&[0xC4; 16]),
                weight_permille: 1000,
            },
            CompositionEntry {
                factor: topic_factor(&[0xC4; 16]),
                weight_permille: -500,
            },
        ];
        assert_eq!(
            validate_composition(&dup),
            Err(CompositionError::DuplicateFactor(topic_factor(&[0xC4; 16])))
        );
    }

    // ── v2 `text-model` artifact (topic-factors.md § Publishing a trained
    // factor, RATIFIED 2026-08-13) ──────────────────────────────────────

    /// Build the raw bytes of a text-model artifact directly from a struct
    /// literal, bypassing [`build_text_model_artifact`] — the only way to hand
    /// `validate_text_model_artifact` a *non-canonical* artifact, which is
    /// exactly what the rejection pins need.
    fn tm_bytes(
        version: u16,
        more_docs: u32,
        less_docs: u32,
        ngrams: Vec<(&str, u32, u32)>,
    ) -> Vec<u8> {
        let artifact = TextModelArtifact {
            version,
            more_docs,
            less_docs,
            ngrams: ngrams
                .into_iter()
                .map(|(ngram, more, less)| TextModelNgram {
                    ngram: ngram.to_string(),
                    more,
                    less,
                })
                .collect(),
            name: None,
        };
        crate::encoding::canonical_encode(&artifact).unwrap()
    }

    #[test]
    fn build_text_model_artifact_canonicalizes_and_round_trips() {
        // Handed over unsorted, as a review sheet might render it.
        let bytes = build_text_model_artifact(
            Some("Small orange cats"),
            4,
            2,
            vec![
                ("orange".into(), 3, 0),
                ("cat".into(), 4, 1),
                ("dog".into(), 1, 2),
            ],
        )
        .unwrap();
        let artifact = validate_text_model_artifact(&bytes).unwrap();
        assert_eq!(artifact.version, TEXT_MODEL_ARTIFACT_VERSION);
        assert_eq!(artifact.name.as_deref(), Some("Small orange cats"));
        assert_eq!(artifact.more_docs, 4);
        assert_eq!(artifact.less_docs, 2);
        let grams: Vec<&str> = artifact.ngrams.iter().map(|n| n.ngram.as_str()).collect();
        assert_eq!(
            grams,
            vec!["cat", "dog", "orange"],
            "the builder owns the ascending canonical order, not the caller"
        );
    }

    #[test]
    fn equal_text_model_vocabularies_encode_to_equal_bytes() {
        // The `wasm_hash` metadata binding is only meaningful if equal artifacts
        // have equal bytes (the List's property, § Publishing's artifact shape).
        let a = build_text_model_artifact(
            Some("Cats"),
            4,
            2,
            vec![("b".into(), 2, 1), ("a".into(), 3, 1)],
        )
        .unwrap();
        let b = build_text_model_artifact(
            Some("Cats"),
            4,
            2,
            vec![("a".into(), 3, 1), ("b".into(), 2, 1)],
        )
        .unwrap();
        assert_eq!(a, b, "input order must not reach the bytes");
    }

    #[test]
    fn validate_text_model_artifact_rejects_a_non_canonical_vocabulary() {
        assert_eq!(
            validate_text_model_artifact(&tm_bytes(1, 3, 3, vec![("b", 3, 0), ("a", 3, 0)])),
            Err(TextModelArtifactError::NotStrictlyAscending(1)),
            "descending"
        );
        assert_eq!(
            validate_text_model_artifact(&tm_bytes(1, 3, 3, vec![("a", 3, 0), ("a", 2, 1)])),
            Err(TextModelArtifactError::NotStrictlyAscending(1)),
            "duplicate"
        );
    }

    #[test]
    fn labeler_factor_id_is_the_inverse_of_labeler_factor() {
        let id = ActorId([0xAB; 32]);
        let factor = labeler_factor(&id);
        assert!(is_labeler_factor(&factor));
        assert_eq!(labeler_factor_id(&factor), Some(id));

        // Not this namespace, and not a well-formed key in it.
        assert!(!is_labeler_factor(&topic_factor(&[0xC4; 16])));
        assert_eq!(labeler_factor_id(&topic_factor(&[0xC4; 16])), None);
        assert_eq!(labeler_factor_id("labeler:not-hex"), None);
        assert_eq!(
            labeler_factor_id("labeler:abcd"),
            None,
            "a short id is not a labeler key — the parser is strict, the \
             namespace check is not"
        );
    }

    #[test]
    fn validate_text_model_artifact_enforces_the_prune_floor_structurally() {
        // ⚠ PRIVACY PIN, and the reason it lives in the *validator* rather than
        // only in the scrub: `more`/`less` ARE the per-class distinct-document
        // counts, so `more + less` is exactly the quantity
        // `TEXT_MODEL_PUBLISH_MIN_DOCS` bounds. Checking it at the nest publish
        // gate turns "the publisher pruned honestly" into a property of the
        // artifact — a buggy or hostile client cannot put a single-document
        // quote of someone's marked post on a public registry.
        assert_eq!(
            validate_text_model_artifact(&tm_bytes(1, 9, 9, vec![("a", 2, 0)])),
            Err(TextModelArtifactError::BelowPruneFloor { index: 0, docs: 2 }),
            "two documents is a quote, not an aggregate"
        );
        // Class-blind: 2 + 1 clears the floor that 2 + 0 does not.
        assert!(validate_text_model_artifact(&tm_bytes(1, 9, 9, vec![("a", 2, 1)])).is_ok());
    }

    #[test]
    fn validate_text_model_artifact_rejects_a_count_above_its_class_doc_counter() {
        // A per-class occurrence count is a count of *documents* of that class,
        // so it can never exceed that class's document counter. A publisher
        // whose scrub produced one has a bug the subscriber must not inherit:
        // the posterior would read a likelihood above 1.
        assert_eq!(
            validate_text_model_artifact(&tm_bytes(1, 2, 5, vec![("a", 3, 0)])),
            Err(TextModelArtifactError::MoreCountAboveDocs {
                index: 0,
                more: 3,
                more_docs: 2
            })
        );
        assert_eq!(
            validate_text_model_artifact(&tm_bytes(1, 5, 2, vec![("a", 0, 3)])),
            Err(TextModelArtifactError::LessCountAboveDocs {
                index: 0,
                less: 3,
                less_docs: 2
            })
        );
    }

    #[test]
    fn validate_text_model_artifact_rejects_an_entry_zero_in_both_classes() {
        // An all-zero entry carries no signal and no disclosure — it is pure
        // bytes, and (unlike an absent n-gram, which the posterior skips) it
        // would still cost a subscriber a lookup. § Publishing pins "each entry
        // non-zero in at least one class".
        assert_eq!(
            validate_text_model_artifact(&tm_bytes(1, 3, 3, vec![("a", 0, 0)])),
            Err(TextModelArtifactError::EmptyEntry(0))
        );
    }

    #[test]
    fn validate_text_model_artifact_rejects_an_over_long_vocabulary() {
        let ngrams: Vec<(String, u32, u32)> = (0..=TEXT_MODEL_PUBLISH_MAX_NGRAMS)
            .map(|i| (format!("{i:05}"), 1, 0))
            .collect();
        let n = ngrams.len();
        assert_eq!(
            build_text_model_artifact(None, 3, 0, ngrams),
            Err(TextModelArtifactError::TooManyNgrams(n)),
            "the vocabulary cap is the REVIEW bound — the vocabulary IS the \
             disclosure, so it is enforced structurally, not only publisher-side"
        );
    }

    #[test]
    fn validate_text_model_artifact_applies_the_lists_name_rules_verbatim() {
        // § Publishing: "name: Option<String> (the List's name rules verbatim)".
        assert_eq!(
            build_text_model_artifact(Some("   "), 3, 0, vec![("a".into(), 3, 0)]),
            Err(TextModelArtifactError::BlankName)
        );
        let long = "x".repeat(MAX_LABELER_LIST_NAME_LEN + 1);
        assert_eq!(
            build_text_model_artifact(Some(&long), 3, 0, vec![("a".into(), 3, 0)]),
            Err(TextModelArtifactError::NameTooLong(
                MAX_LABELER_LIST_NAME_LEN + 1
            ))
        );
        let unnamed = build_text_model_artifact(None, 3, 0, vec![("a".into(), 3, 0)]).unwrap();
        assert_eq!(
            validate_text_model_artifact(&unnamed).unwrap().name,
            None,
            "an unnamed model is valid — the name is optional, as on a List"
        );
    }

    #[test]
    fn validate_text_model_artifact_accepts_an_unknown_future_version() {
        // ⚠ COMPAT PIN. The nest publish gate runs this validator, so rejecting
        // an unrecognized `version` here would make an OLDER nest refuse a
        // NEWER client's artifact — exactly the bidirectional break
        // `version-compatibility.md` forbids within a major version. The
        // version is the *tokenizer* contract, and its consumer is the
        // subscriber's scorer, which goes INERT on an unknown version
        // (frame § Tier-3 artifact kinds) rather than mis-scoring. Structure is
        // this validator's job; interpretation is the scorer's.
        let future = tm_bytes(TEXT_MODEL_ARTIFACT_VERSION + 7, 3, 0, vec![("a", 3, 0)]);
        let artifact = validate_text_model_artifact(&future)
            .expect("an unknown version is structurally valid, not malformed");
        assert_eq!(artifact.version, TEXT_MODEL_ARTIFACT_VERSION + 7);
        assert_eq!(
            validate_text_model_artifact(&tm_bytes(0, 3, 0, vec![("a", 3, 0)])),
            Err(TextModelArtifactError::ZeroVersion),
            "version 0 is not a future version — it is an unstamped artifact"
        );
    }

    #[test]
    fn validate_text_model_artifact_rejects_a_non_v1_shaped_ngram() {
        // ⚠ PRIVACY PIN:
        // an n-gram longer than 3 tokens can never match a subscriber's v1
        // tokenizer, so it scores nothing and is pure disclosure of the
        // publisher's marked-post text.
        assert_eq!(
            validate_text_model_artifact(&tm_bytes(1, 3, 3, vec![("a b c d", 3, 0)])),
            Err(TextModelArtifactError::InvalidNgramShape(0)),
            "4 tokens exceeds the v1 1-3-gram contract"
        );
        assert_eq!(
            validate_text_model_artifact(&tm_bytes(
                1,
                3,
                3,
                vec![(
                    "this is a fourteen token sentence that leaks the marked post text",
                    3,
                    0
                )]
            )),
            Err(TextModelArtifactError::InvalidNgramShape(0)),
            "a sentence-length quote is exactly the out-of-contract shape this check exists to refuse"
        );
        assert!(
            validate_text_model_artifact(&tm_bytes(1, 3, 3, vec![("a b c", 3, 0)])).is_ok(),
            "a real 3-gram passes"
        );
    }

    #[test]
    fn validate_text_model_artifact_rejects_doubled_leading_and_trailing_spaces() {
        assert_eq!(
            validate_text_model_artifact(&tm_bytes(1, 3, 3, vec![("a  b", 3, 0)])),
            Err(TextModelArtifactError::InvalidNgramShape(0)),
            "doubled space"
        );
        assert_eq!(
            validate_text_model_artifact(&tm_bytes(1, 3, 3, vec![(" a b", 3, 0)])),
            Err(TextModelArtifactError::InvalidNgramShape(0)),
            "leading space"
        );
        assert_eq!(
            validate_text_model_artifact(&tm_bytes(1, 3, 3, vec![("a b ", 3, 0)])),
            Err(TextModelArtifactError::InvalidNgramShape(0)),
            "trailing space"
        );
    }

    #[test]
    fn validate_text_model_artifact_ngram_shape_is_not_checked_at_an_unknown_version() {
        // Compat arm: the shape is *this build's* v1 contract, not a property
        // an unrecognized future version must share (§ Compat note above).
        assert!(
            validate_text_model_artifact(&tm_bytes(
                TEXT_MODEL_ARTIFACT_VERSION + 1,
                3,
                3,
                vec![("a b c d", 3, 0)]
            ))
            .is_ok(),
            "a 4-token n-gram at version 2 is structure this build cannot judge"
        );
    }

    #[test]
    fn the_text_model_artifact_layer_accepts_an_empty_vocabulary() {
        // Mirrors the List exactly: an empty map is canonically well-formed, so
        // the *artifact* layer takes it and the **publish lifecycle** owns the
        // refusal (`PublishListError::NoEntries`'s twin). Keeping the refusal
        // one layer up is what stops seven app shells from each re-deciding it.
        let bytes = build_text_model_artifact(None, 0, 0, vec![]).unwrap();
        assert!(
            validate_text_model_artifact(&bytes)
                .unwrap()
                .ngrams
                .is_empty()
        );
    }
}

/// The open arms of [`FilterRule`] and [`crate::data::BodyHint`]
/// (`transport.md` § Rule 3 in full, *Open, carrying*), proven against
/// test-only twins standing in for a NEWER writer.
#[cfg(test)]
mod unknown_arm_tests {
    use super::*;
    use crate::encoding::{canonical_decode, canonical_encode};

    #[derive(Serialize)]
    enum NewerBodyHint {
        Text,
        Hologram,
    }

    /// A newer rule set: two rules this build knows (one holding a newer hint)
    /// and one whole rule it has never heard of.
    #[derive(Serialize)]
    enum NewerFilterRule {
        HasMedia { required: bool },
        BodyHint { hints: Vec<NewerBodyHint> },
        HasPoll { required: bool, min_options: u32 },
    }

    fn newer_rules() -> Vec<NewerFilterRule> {
        vec![
            NewerFilterRule::HasMedia { required: true },
            NewerFilterRule::BodyHint {
                hints: vec![NewerBodyHint::Text, NewerBodyHint::Hologram],
            },
            NewerFilterRule::HasPoll {
                required: true,
                min_options: 3,
            },
        ]
    }

    #[test]
    fn a_rule_set_with_unknown_rules_decodes_and_round_trips_byte_identically() {
        let bytes = canonical_encode(&newer_rules()).unwrap();
        let rules: Vec<FilterRule> = canonical_decode(&bytes).expect("the rule set decodes");

        assert_eq!(rules[0], FilterRule::HasMedia { required: true });
        assert_eq!(
            rules[1],
            FilterRule::BodyHint {
                hints: vec![
                    crate::data::BodyHint::Text,
                    crate::data::BodyHint::Other("Hologram".into()),
                ],
            }
        );
        assert!(matches!(rules[2], FilterRule::Unknown(_)));
        assert!(rules[0].is_known() && rules[1].is_known() && !rules[2].is_known());

        // Carrying: the nest stores, `feed.get` returns and `feed.update`
        // echoes exactly the bytes the newer writer produced.
        assert_eq!(canonical_encode(&rules).unwrap(), bytes);
    }

    /// The known spellings of a hint still land on their own arms.
    #[test]
    fn a_known_hint_is_not_swallowed_by_the_carry() {
        let bytes = canonical_encode(&vec![NewerBodyHint::Text]).unwrap();
        let hints: Vec<crate::data::BodyHint> = canonical_decode(&bytes).unwrap();
        assert_eq!(hints, vec![crate::data::BodyHint::Text]);
    }
}
