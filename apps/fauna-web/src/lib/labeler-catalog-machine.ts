// Shared snapshot shapes for the page-level `LabelerCatalogMachine`
// (`libs/fauna-labeler-catalog-machine`, WASM twin
// `libs/fauna-wasm-labeler-catalog`), consumed by the Personalization home
// (`$lib/components/PersonalizationSection.svelte`) and the Community
// labelers catalog (`$lib/components/LabelerCatalogSection.svelte`) — both
// render off the SAME `LabelerCatalogSnapshot` the one shared machine owns,
// the personalization home just client-filtering `entries` to
// `subscribed === true`. See
// docs/goal/architecture/content-moderation-and-ranking.md § Tier-3.
//
// Mirrors `fauna_labeler_catalog_machine::snapshots::{LabelerCatalogEntry,
// LabelerInspectView, LabelerCatalogSnapshot}` serde JSON (snake_case across
// the serde_json boundary).
import type { LocalizedText } from '$lib/i18n/localized';

/** One community labeler in the catalog (`labeler-catalog-item` row). */
export interface LabelerCatalogEntry {
  /** Hex-encoded 32-byte labeler id (== the signer's `algorithm_id`). */
  labeler_id: string;
  version: number;
  /** Hex-encoded 32-byte publisher actor id. */
  publisher_actor: string;
  /** The artifact kind (`"wasm"` | `"list"` | `"text-model"`;
   *  content-moderation-and-ranking.md § Tier-3 artifact kinds) — what
   *  `labeler-catalog-item-kind` renders, so a curated List is
   *  distinguishable from an executable module before inspect. Transcribed
   *  verbatim from the nest (which refuses an empty kind at publish). */
  artifact_kind: string;
  /** `text-model` kind only: its tokenizer/schema version, as the nest
   *  projected it off the artifact at publish
   *  (`fauna_protocol::labelers::LabelerSummary::artifact_version`). `0` =
   *  absent (any non-text-model kind) — NOT "version zero". Feed to
   *  `textModelNeedsNewerApp`, never compared directly. */
  artifact_version: number;
  /** `content.read{kind}` this labeler scores (`post` | `mail`). */
  content_kind: string;
  /** The `content_scores.factor` it writes (`labeler:<hex>`). */
  factor: string;
  /** Hex-encoded 36-byte WASM content hash. */
  wasm_hash: string;
  wasm_size: number;
  /** Whether the caller currently subscribes to this labeler. */
  subscribed: boolean;
}

/** One entry of an inspected List artifact (`labeler-inspect-list-entry`). */
export interface LabelerInspectListEntry {
  /** Hex-encoded 32-byte content id. */
  content_id: string;
  /** The publisher's per-mille score ∈ [0,1000], verbatim from the artifact. */
  score: number;
}

/** One n-gram of an inspected `text-model` artifact
 *  (`labeler-inspect-model-entry`) — the `LabelerInspectListEntry` twin. */
export interface LabelerInspectModelNgram {
  /** The n-gram itself: 1–3 tokenizer tokens joined by single spaces. */
  ngram: string;
  /** Distinct *more like this* training documents it occurred in. */
  more: number;
  /** Distinct *less like this* training documents it occurred in. */
  less: number;
}

/** The decoded, re-verified signed metadata for one labeler — the
 *  inspect-before-subscribe trust-gate view. */
export interface LabelerInspectView {
  labeler_id: string;
  version: number;
  /** The artifact kind (`"wasm"` | `"list"`), transcribed like
   *  {@link LabelerCatalogEntry.artifact_kind}. Decides which inspect facts
   *  apply: `list_name`/`list_entries` are populated only for `"list"`. */
  artifact_kind: string;
  wasm_hash: string;
  wasm_size: number;
  needs_text: boolean;
  needs_hashtags: boolean;
  needs_media_metadata: boolean;
  needs_author: boolean;
  /** Whether this module reads raw attachment bytes (image/video content),
   *  beside the four v1 flags — the fifth transparency line
   *  (content-moderation-and-ranking.md § Tier-3 → *The attachment facet*). */
  needs_attachment_bytes: boolean;
  /** Whether the client-side re-verify (`verify_labeler_metadata`) accepted
   *  the hash/size/signature binding — `false` renders a tampering warning. */
  verified: boolean;
  /** `list` kind only: the decoded publisher-chosen public name. It rides
   *  **inside** the artifact (`topic-factors.md` § Publishing — zero new
   *  wire); `null` for an unnamed list and for every `wasm` labeler. */
  list_name: string | null;
  /** `list` kind only: the artifact's **exact** id→score map, every entry
   *  (content-moderation-and-ranking.md § Tier-3 artifact kinds: "the client
   *  renders the exact id→score map before subscribing"). Empty for `wasm`
   *  labelers and for an unverifiable list. */
  list_entries: LabelerInspectListEntry[];
  /** `text-model` kind only: the decoded publisher-chosen public name — the
   *  `list_name` twin. `null` for an unnamed model and for every other kind. */
  model_name: string | null;
  /** `text-model` kind only: the artifact's **whole** vocabulary, every
   *  entry, never a capped preview — the `list_entries` twin. Empty for
   *  every other kind and for an unverifiable model. */
  model_ngrams: LabelerInspectModelNgram[];
}

/** The whole renderable labeler-catalog page in one record. */
export interface LabelerCatalogSnapshot {
  entries: LabelerCatalogEntry[];
  /** `Some` while the inspect panel is open; `null` otherwise. */
  inspecting: LabelerInspectView | null;
  /** Page-level `error-message` (last gesture/refresh failure); `null` when clear. */
  error: LocalizedText | null;
  /** Whether a `refresh()` has ever returned successfully — gates
   *  `labeler-catalog-empty`/`personalization-labelers-empty` (README.md §
   *  List pages: loading is not empty). */
  loaded: boolean;
}
