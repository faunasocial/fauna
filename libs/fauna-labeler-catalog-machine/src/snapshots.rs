//! The page-level renderable `LabelerCatalogSnapshot` + its sub-types. Clients
//! read a fresh copy on every observer tick and render the whole
//! labeler-catalog page (plus the personalization home's subscribed-labelers
//! facet, which filters this same snapshot to `subscribed == true` rows) off
//! it; they never see the internal state. Mirrors `fauna_devices_machine::snapshots`.

use serde::{Deserialize, Serialize};

use fauna_core::localized::LocalizedText;

/// One community labeler in the catalog (`labeler-catalog-item` /
/// `personalization-labelers-list` row). Transcribes the `fauna.labelers.list`
/// row (`fauna_protocol::labelers::LabelerSummary`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct LabelerCatalogEntry {
    /// Hex-encoded 32-byte labeler id (== the signer's `algorithm_id`).
    pub labeler_id: String,
    pub version: u64,
    /// Hex-encoded 32-byte publisher actor id.
    pub publisher_actor: String,
    /// The artifact kind (`"wasm"` | `"list"`; content-moderation-and-ranking.md
    /// § Tier-3 artifact kinds) — what `labeler-catalog-item-kind` renders, so a
    /// curated List is distinguishable from an executable module before
    /// inspect. Transcribed verbatim from the nest (which stores the kind the
    /// publisher declared and refuses an empty one) — clients render it as-is.
    pub artifact_kind: String,
    /// For a `"text-model"` artifact: its tokenizer/schema version, as the nest
    /// projected it off the artifact at publish
    /// (`fauna_protocol::labelers::LabelerSummary::artifact_version`). `0` =
    /// absent (any non-text-model kind) — *not* "version zero".
    ///
    /// Carried on the **browse** entry rather than fetched per row on purpose:
    /// the version lives inside the artifact bytes, which only `inspect`
    /// returns, and refreshing a catalog must not cost N × 64 KiB round trips.
    /// Clients feed it to `fauna_core::format::text_model_needs_newer_app`
    /// rather than comparing it themselves.
    pub artifact_version: u64,
    /// `content.read{kind}` this labeler scores (`post` | `mail`).
    pub content_kind: String,
    /// The `content_scores.factor` it writes (`labeler:<hex>`).
    pub factor: String,
    /// Hex-encoded 36-byte content hash of the artifact bytes (the WASM module
    /// for `wasm`, the dag-cbor List for `list` — the wire field name is kept
    /// for compat; read "artifact hash").
    pub wasm_hash: String,
    pub wasm_size: u64,
    /// Whether the caller currently subscribes to this labeler
    /// (`fauna_protocol::labelers::LabelerSummary::subscribed`).
    pub subscribed: bool,
}

/// The decoded, re-verified signed metadata for one labeler — the
/// inspect-before-subscribe trust-gate view (`labeler-inspect-panel` /
/// `labeler-inspect-metadata`). Transcribes the decoded
/// `fauna_core::scoring::AlgorithmLabeler` (`inspect`'s `metadata_blob`), never
/// the raw WASM bytes themselves (those stay behind the FFI boundary that
/// executes them — the panel shows facts about the module, not its source).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct LabelerInspectView {
    pub labeler_id: String,
    pub version: u64,
    /// The artifact kind (`"wasm"` | `"list"`), transcribed like
    /// [`LabelerCatalogEntry::artifact_kind`]. Decides which inspect facts
    /// apply: the `needs_*` input schema is a `wasm` declaration (a List has no
    /// inputs — its metadata carries mandatory dummies), and
    /// [`list_name`](Self::list_name)/[`list_entries`](Self::list_entries) are
    /// populated only here for `"list"`.
    pub artifact_kind: String,
    pub wasm_hash: String,
    pub wasm_size: u64,
    /// The declared `label()` input requirements
    /// (`fauna_core::scoring::LabelerInput`) — what the module reads from a
    /// scored item.
    pub needs_text: bool,
    pub needs_hashtags: bool,
    pub needs_media_metadata: bool,
    pub needs_author: bool,
    /// The module reads attachment **bytes** (`LabelerInput::needs_attachment_bytes`,
    /// the fifth declared input — `content-moderation-and-ranking.md` § Tier-3
    /// → *The attachment facet*). Additive on the record: an app built before
    /// it reads `false`.
    #[cfg_attr(feature = "uniffi", uniffi(default = false))]
    pub needs_attachment_bytes: bool,
    /// Whether `fauna_core::scoring::verify_labeler_metadata` accepted the
    /// hash/size/signature binding — re-run client-side (not just trusted from
    /// the nest) so the transparency gate holds even against a
    /// compromised/lying nest. For a `list` artifact this additionally requires
    /// `validate_list_artifact` to accept the bytes: an undecodable List cannot
    /// be inspected, and what cannot be inspected must not read as verified.
    /// `false` renders a tampering warning instead of enabling subscribe.
    pub verified: bool,
    /// `list` kind only: the decoded publisher-chosen public name. It rides
    /// **inside** the artifact (`topic-factors.md` § Publishing — zero new
    /// wire), so inspect is where it first becomes visible; `None` for an
    /// unnamed list and for every `wasm` labeler.
    pub list_name: Option<String>,
    /// `list` kind only: the artifact's **exact** id→score map, every entry
    /// (content-moderation-and-ranking.md § Tier-3 artifact kinds: "the client
    /// renders the exact id→score map before subscribing" — a capped preview
    /// is not the exact map). Empty for `wasm` labelers and for an
    /// unverifiable list.
    pub list_entries: Vec<LabelerInspectListEntry>,
    /// `text-model` kind only: the decoded publisher-chosen public name. Rides
    /// **inside** the artifact exactly as [`list_name`](Self::list_name) does
    /// (`topic-factors.md` § Publishing — the same name rules, and the same
    /// consequence that inspect is where a name first becomes visible); `None`
    /// for an unnamed model and for every other kind.
    pub model_name: Option<String>,
    /// `text-model` kind only: the artifact's **whole** vocabulary
    /// (content-moderation-and-ranking.md § Tier-3 artifact kinds: inspect
    /// renders the full vocabulary — the model's entire matching surface —
    /// before subscribing). Every entry, never a capped preview: a truncated
    /// vocabulary is not the artifact, and unlike a List — whose entries are
    /// ids the publisher already made public — this vocabulary *is* what the
    /// subscriber is being asked to trust. `TEXT_MODEL_PUBLISH_MAX_NGRAMS`
    /// bounds the artifact, so a full render is always feasible. Empty for
    /// every other kind and for an unverifiable model.
    pub model_ngrams: Vec<LabelerInspectModelNgram>,
}

/// One n-gram of an inspected `text-model` artifact
/// (`labeler-inspect-model-entry`) — the renderable form of
/// `fauna_core::scoring::TextModelNgram`.
///
/// Carries the two **per-class distinct-document counts** rather than a
/// pre-rendered direction word, because the direction and the count are shared
/// value-formatting faces (`fauna_core::format::{ngram_direction_label,
/// ngram_doc_count_label}`) — the same two the publisher's review rows use. A
/// machine that pre-rendered them would fork the publisher's promise from what
/// the subscriber reads back, which is precisely the drift the shared faces
/// exist to prevent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct LabelerInspectModelNgram {
    /// The n-gram itself: 1–3 tokenizer tokens joined by single spaces.
    pub ngram: String,
    /// Distinct *more like this* training documents it occurred in.
    pub more: u32,
    /// Distinct *less like this* training documents it occurred in.
    pub less: u32,
}

/// One entry of an inspected List artifact (`labeler-inspect-list-entry`) —
/// the renderable form of `fauna_core::scoring::ListEntry`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct LabelerInspectListEntry {
    /// Hex-encoded 32-byte content id.
    pub content_id: String,
    /// The publisher's per-mille score ∈ [0,1000], verbatim from the artifact.
    pub score: i64,
}

/// The whole renderable labeler-catalog page in one record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct LabelerCatalogSnapshot {
    pub entries: Vec<LabelerCatalogEntry>,
    /// `Some` while the inspect panel is open (the last `inspect()` result);
    /// `None` otherwise.
    pub inspecting: Option<LabelerInspectView>,
    /// The page-level `error-message` (last gesture / refresh failure),
    /// localized client-side; `None` when clear.
    pub error: Option<LocalizedText>,
    /// Whether a [`LabelerCatalogMachine::refresh`] has ever **returned
    /// successfully** — the loaded-vs-still-loading bit, and the second painting
    /// condition of BOTH empty-state elements this snapshot feeds:
    /// `labeler-catalog-empty` on the catalog page and
    /// `personalization-labelers-empty` on the Personalization home's subscribed
    /// facet (the same `entries`, client-filtered to `subscribed == true`).
    ///
    /// The general rule and its rationale are
    /// `docs/goal/ui/README.md` § *List pages: loading is not empty* —
    /// `entries` alone cannot answer "are there no community labelers?", because
    /// it is empty both before the first `fauna.labelers.list` returns and after
    /// one that found nothing. Every app painted "No community labelers
    /// published yet" over a catalog nobody had read yet.
    ///
    /// **Monotonic**: set on the first successful refresh and never cleared. A
    /// *failed* refresh leaves it as it was — a first-read failure keeps the page
    /// unloaded, so `error-message` does the talking instead of a false empty
    /// state beside it, and a later failure keeps prior rows on screen, so the
    /// loading state is never re-armed under visible rows.
    ///
    /// [`LabelerCatalogMachine::refresh`]: crate::LabelerCatalogMachine::refresh
    pub loaded: bool,
}
