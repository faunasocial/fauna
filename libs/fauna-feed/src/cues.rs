//! Engagement-cue derivation + the sealed per-item cue rollup
//! (`docs/goal/behavior/engagement-cues.md` — the owner doc). Phase 3, Layer A
//! plumbing: the client-side, shared-Rust half of "I watched this cat video to
//! the end" / "I scrolled straight past this fish".
//!
//! # The split this module makes concrete
//!
//! Raw visibility/scroll/playback *sampling* is client glue — each platform's
//! intersection-observer and media-playback APIs differ — so a shell reports
//! per-exposure [`CueObservation`]s. **All *derivation* is here, in shared Rust**
//! ([`CueEngine`]), so the two coarse verdicts can never drift per client
//! (owner doc § Cue vocabulary & derivation). Every threshold the derivation
//! applies is a hard-coded constant in [`fauna_core::scoring::cues`]; only the
//! sampling that produces a [`CueObservation`] lives in the shells.
//!
//! # The two v1 verdicts (deliberately coarse — two, not twenty)
//!
//! * [`CueVerdict::WatchComplete`] — media played ≥ `CUE_COMPLETE_FRACTION_PM`
//!   of its duration, **or** a non-media post held ≥ `CUE_LONG_DWELL_VISIBLE_PM`
//!   visible for ≥ `CUE_DWELL_LONG_MS`.
//! * [`CueVerdict::Skip`] — the item was ≥ `CUE_SKIP_VISIBLE_PM` visible for
//!   *less than* `CUE_SKIP_MS` before scrolling on, **and** the surrounding
//!   session shows normal reading pace (a fast-scroll burst — consecutive
//!   exposures closer than `CUE_BURST_MIN_GAP_MS` — derives nothing; flick-
//!   throughs are not judgments).
//!
//! Verdicts are **per-(user, item), idempotent, last-wins** — re-watching flips
//! a `Skip` to a `WatchComplete`. There is no event *stream*, only a current
//! verdict per item, accumulated into a [`CueRollup`].
//!
//! # At rest (owner doc § At rest)
//!
//! The [`CueRollup`] is sealed under the personalization plane's own key —
//! the delegable unit for `fauna.personalization.model`, derived from the
//! user's BackupKey (`fauna_client_personalization::model_seal_keys`) — and
//! synced verbatim-opaque through the existing `fauna.personalization.model.*` wire under the factor key
//! [`fauna_core::scoring::CUES_ROLLUP_FACTOR_V1`], purely for cross-device
//! continuity + reinstall survival. It is **not** a composition factor: the nest
//! never folds it into a feed (`fauna_core::scoring::is_topic_factor` rejects
//! `cues:`), it only stores the opaque blob. Capacity is bounded to
//! `CUE_ROLLUP_MAX_ITEMS` (oldest `last_at` evicted); the seal additionally
//! caps the blob at the wire's 512 KiB (owner doc § Seal + home).

use std::collections::BTreeMap;

use fauna_client_personalization::{
    ModelSealError, ModelSealKey, seal_model_bytes, unseal_model_bytes,
};
use fauna_core::scoring::cues::{
    CUE_BURST_MIN_GAP_MS, CUE_COMPLETE_FRACTION_PM, CUE_DWELL_LONG_MS, CUE_ROLLUP_MAX_ITEMS,
    CUE_SKIP_MS,
};
use serde::{Deserialize, Serialize};

/// The current cue-layout version stamped into a serialized [`CueRollup`].
/// Additive-only within a major version (owner doc § At rest — "serialized
/// additively"): a newer client widens [`ItemCues`] with defaulted fields and an
/// older client drops what it does not know, never a schema break. Bumping this
/// is reserved for the (unlikely) day the *shape* changes incompatibly, which
/// within a major version it may not.
const CUE_ROLLUP_VERSION: u16 = 1;

/// A derived, coarse per-item engagement verdict (owner doc § Cue vocabulary).
///
/// Serialized as `"watch-complete"` / `"skip"` — a compat surface (these strings
/// rest inside the user's sealed rollup and cross devices), so the `rename_all`
/// below MUST NOT change. Adding a verdict later is additive vocabulary growth
/// (owner doc: "a future cue kind is a new derived verdict + rollup field, never
/// a schema break"), and an older client already tolerates the new string: it
/// lands in [`Self::Other`] and is carried through the re-seal
/// (`transport.md` § Schema and forward-compat discipline → *Rule 3 in full*).
///
/// Not a UniFFI type: no app reads a verdict — the manager derives and folds
/// them, and the FFI's `record_observation` returns nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CueVerdict {
    /// The user consumed the item to (near) completion.
    WatchComplete,
    /// The user scrolled past the item after only a brief, deliberate glance.
    Skip,
    /// A verdict a newer build derives and this one does not name — the exact
    /// string read, carried so this build's re-seal of the rollup keeps it. It
    /// trains nothing, is contributed nowhere and counts no completion; a
    /// fresh observation of the item replaces it last-wins, as any verdict
    /// is replaced. This build never derives one.
    #[serde(untagged)]
    Other(String),
}

impl CueVerdict {
    /// The wire string the Layer-B `fauna.moderation.signal_contribute` write
    /// path expects (owner doc § Layer B) — identical to the kebab-case serde
    /// rename above, but a direct `&'static str` so the producer never round-trips
    /// through serde to name a verdict it already holds. `None` for
    /// [`Self::Other`]: a verdict this build does not name is never
    /// contributed.
    pub fn wire_str(&self) -> Option<&'static str> {
        match self {
            CueVerdict::WatchComplete => Some("watch-complete"),
            CueVerdict::Skip => Some("skip"),
            CueVerdict::Other(_) => None,
        }
    }
}

/// The accumulated cue state for one item (owner doc § At rest —
/// `ItemCues { verdict, watch_ms_total, completions, last_at }`).
///
/// `#[serde(default)]` on every field keeps the type additively readable: an
/// older client reading a newer rollup simply ignores fields it lacks, and a
/// newer client reading an older one fills absent fields with their defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemCues {
    /// The current (last-wins) verdict for this item.
    pub verdict: CueVerdict,
    /// Cumulative substantial-visibility milliseconds observed across every
    /// exposure that produced a verdict for this item. A stored statistic; not
    /// consumed by the v1 Layer-A training path (which reads only `verdict`).
    #[serde(default)]
    pub watch_ms_total: u64,
    /// How many times a `WatchComplete` has been derived for this item
    /// (re-watches accumulate here even as `verdict` stays `WatchComplete`).
    #[serde(default)]
    pub completions: u32,
    /// The observation time (shell-supplied ms) of the most recent update — the
    /// eviction key (oldest `last_at` is dropped past the capacity cap).
    #[serde(default)]
    pub last_at: u64,
}

/// A bounded, sealed-at-rest map of per-item engagement cues (owner doc § At
/// rest). Held live inside a [`CueEngine`]; sealed + synced by
/// `fauna_client_personalization` under [`fauna_core::scoring::CUES_ROLLUP_FACTOR_V1`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CueRollup {
    /// Layout version (see [`CUE_ROLLUP_VERSION`]).
    #[serde(default)]
    version: u16,
    /// Per-item cues, keyed by content id (the feed's `post_id`). `BTreeMap` for
    /// a deterministic serialization — the sealed bytes must be reproducible so a
    /// no-op re-seal is a no-op put.
    #[serde(default)]
    items: BTreeMap<String, ItemCues>,
}

impl Default for CueRollup {
    fn default() -> Self {
        CueRollup {
            version: CUE_ROLLUP_VERSION,
            items: BTreeMap::new(),
        }
    }
}

impl CueRollup {
    /// A fresh, empty rollup (a mail-less or brand-new actor). Inert until the
    /// first verdict lands.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether any item has a recorded verdict.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The number of items with a recorded verdict.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// The accumulated cues for one item, if any.
    pub fn get(&self, content_id: &str) -> Option<&ItemCues> {
        self.items.get(content_id)
    }

    /// The current verdict for one item, if any — the render/training read.
    pub fn verdict(&self, content_id: &str) -> Option<CueVerdict> {
        self.items.get(content_id).map(|c| c.verdict.clone())
    }

    /// Iterate `(content_id, cues)` in deterministic key order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &ItemCues)> {
        self.items.iter()
    }

    /// Fold one derived verdict for `content_id` into the rollup, **last-wins**:
    /// the verdict overwrites any prior one, `watch_ms` accumulates into
    /// `watch_ms_total`, a `WatchComplete` bumps `completions`, and `last_at`
    /// advances to `observed_at_ms`. Past [`CUE_ROLLUP_MAX_ITEMS`] the oldest
    /// (smallest `last_at`) entry is evicted.
    pub fn record(
        &mut self,
        content_id: &str,
        verdict: CueVerdict,
        watch_ms: u64,
        observed_at_ms: u64,
    ) {
        let entry = self
            .items
            .entry(content_id.to_string())
            .or_insert_with(|| ItemCues {
                verdict: verdict.clone(),
                watch_ms_total: 0,
                completions: 0,
                last_at: observed_at_ms,
            });
        let complete = verdict == CueVerdict::WatchComplete;
        entry.verdict = verdict;
        entry.watch_ms_total = entry.watch_ms_total.saturating_add(watch_ms);
        if complete {
            entry.completions = entry.completions.saturating_add(1);
        }
        entry.last_at = observed_at_ms;
        self.evict_to_cap();
    }

    /// Delete one item's cues (e.g. the content is gone). Returns whether an
    /// entry was removed.
    pub fn forget(&mut self, content_id: &str) -> bool {
        self.items.remove(content_id).is_some()
    }

    /// Evict oldest-`last_at` entries until at most [`CUE_ROLLUP_MAX_ITEMS`]
    /// remain. `record` adds one entry at a time so this normally drops zero or
    /// one, but the loop is robust to a rollup loaded already over-cap.
    fn evict_to_cap(&mut self) {
        while self.items.len() > CUE_ROLLUP_MAX_ITEMS {
            // The eviction key is `last_at`; ties broken by content id so the
            // choice is deterministic (reproducible sealed bytes).
            if let Some(oldest) = self
                .items
                .iter()
                .min_by(|a, b| a.1.last_at.cmp(&b.1.last_at).then_with(|| a.0.cmp(b.0)))
                .map(|(k, _)| k.clone())
            {
                self.items.remove(&oldest);
            } else {
                break;
            }
        }
    }

    /// Canonical bytes for sealing (serde_json, mirroring the topic-model
    /// precedent — the sealed blob is opaque on the wire, so JSON's readability
    /// costs nothing and its additive field semantics are what we want).
    pub fn to_bytes(&self) -> Vec<u8> {
        // A `BTreeMap` + plain scalar fields cannot fail to serialize.
        serde_json::to_vec(self).expect("CueRollup is always serializable")
    }

    /// Parse sealed bytes back into a rollup, or `None` if they are not a valid
    /// rollup encoding. Unknown *future* fields are ignored (additive-forward);
    /// only a structurally invalid blob is rejected. A rejected blob is a
    /// surfaced error at the call site, **never** silently a fresh rollup — that
    /// would erase every cue the user's other devices recorded on the next put.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }
}

/// One per-exposure engagement observation, emitted when an item leaves the
/// viewport (or its media ends). The *thresholds* the fields feed live in
/// [`fauna_core::scoring::cues`].
///
/// **Assembled by [`crate::cue_tracker::CueTracker`], not by a client shell**
/// (boundary revised 2026-07-29 — owner doc § Cue vocabulary & derivation). A
/// shell supplies only its raw geometry probe; all bookkeeping that turns probe
/// readings into these fields is shared. The 2026-07-12 original drew the line
/// here instead, and four hand-written per-app copies of that bookkeeping had
/// already drifted before the revision landed.
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CueObservation {
    /// The content id (the feed's `post_id`).
    pub content_id: String,
    /// Whether the item is a media (video/audio) post — selects the
    /// `WatchComplete` gate (playback fraction vs. long dwell).
    pub is_media: bool,
    /// Peak playback fraction reached, **per-mille**, for a media item; `None`
    /// for a non-media item (or media that never played).
    pub media_played_pm: Option<u32>,
    /// Cumulative ms the item was at least `CUE_SKIP_VISIBLE_PM` visible — the
    /// gate the `Skip` heuristic reads (the shell buckets at that fraction).
    pub dwell_ms_at_skip_visibility: u64,
    /// Cumulative ms the item was at least `CUE_LONG_DWELL_VISIBLE_PM` visible —
    /// the gate the non-media `WatchComplete` heuristic reads.
    pub dwell_ms_at_long_visibility: u64,
    /// The shell-supplied observation time (ms). The engine never reads a clock:
    /// it is fed the real event time (testability + honest pacing).
    pub observed_at_ms: u64,
}

/// The shared-Rust derivation engine (owner doc § Cue vocabulary & derivation).
/// Wraps a persisted [`CueRollup`] plus transient session state (the previous
/// exposure time, for burst detection). Constructed from the fetched sealed
/// rollup at session start; fed [`CueObservation`]s as the user scrolls; read for
/// its rollup at put/debounce time.
#[derive(Debug, Clone)]
pub struct CueEngine {
    rollup: CueRollup,
    /// The `observed_at_ms` of the previous exposure, if any — used only to
    /// suppress `Skip` during a fast-scroll burst. Transient (not persisted): a
    /// fresh session legitimately starts with no pacing history.
    last_exposure_at_ms: Option<u64>,
}

impl CueEngine {
    /// An engine over a freshly-fetched (or empty) rollup, with no pacing history.
    pub fn new(rollup: CueRollup) -> Self {
        CueEngine {
            rollup,
            last_exposure_at_ms: None,
        }
    }

    /// An engine over an empty rollup (a fresh actor).
    pub fn empty() -> Self {
        Self::new(CueRollup::new())
    }

    /// Borrow the live rollup (the seal/put read).
    pub fn rollup(&self) -> &CueRollup {
        &self.rollup
    }

    /// Consume the engine for its rollup.
    pub fn into_rollup(self) -> CueRollup {
        self.rollup
    }

    /// The current verdict for one item, if any — the render/training read.
    pub fn verdict(&self, content_id: &str) -> Option<CueVerdict> {
        self.rollup.verdict(content_id)
    }

    /// Derive a verdict from one exposure without touching state — the pure heart
    /// of the engine, separated so the thresholds are testable in isolation.
    /// `prev_exposure_at_ms` is the previous exposure's `observed_at_ms` (for the
    /// burst gate); `None` when this is the session's first exposure.
    fn derive(obs: &CueObservation, prev_exposure_at_ms: Option<u64>) -> Option<CueVerdict> {
        // WatchComplete dominates: re-watching flips a Skip to WatchComplete, and
        // a completed item is never simultaneously a skip.
        let complete = if obs.is_media {
            obs.media_played_pm
                .is_some_and(|pm| pm >= CUE_COMPLETE_FRACTION_PM)
        } else {
            obs.dwell_ms_at_long_visibility >= CUE_DWELL_LONG_MS
        };
        if complete {
            return Some(CueVerdict::WatchComplete);
        }

        // Skip: the item was really-but-briefly seen, and not mid-flick. The
        // item must have *reached* skip-visibility (dwell > 0 at that fraction)
        // yet held it for less than CUE_SKIP_MS.
        let briefly_seen =
            obs.dwell_ms_at_skip_visibility > 0 && obs.dwell_ms_at_skip_visibility < CUE_SKIP_MS;
        let normal_pace = prev_exposure_at_ms
            .is_none_or(|prev| obs.observed_at_ms.saturating_sub(prev) >= CUE_BURST_MIN_GAP_MS);
        if briefly_seen && normal_pace {
            return Some(CueVerdict::Skip);
        }
        None
    }

    /// Feed one exposure to the engine: derive a verdict, fold it into the
    /// rollup (last-wins), advance the pacing clock, and return what (if
    /// anything) was derived. The pacing clock advances for **every** exposure —
    /// including verdict-less ones — so a burst is detected from the full
    /// exposure cadence, not just the ones that scored.
    pub fn observe(&mut self, obs: CueObservation) -> Option<CueVerdict> {
        let verdict = Self::derive(&obs, self.last_exposure_at_ms);
        if let Some(v) = verdict.clone() {
            let watch_ms = obs
                .dwell_ms_at_skip_visibility
                .max(obs.dwell_ms_at_long_visibility);
            self.rollup
                .record(&obs.content_id, v, watch_ms, obs.observed_at_ms);
        }
        self.last_exposure_at_ms = Some(obs.observed_at_ms);
        verdict
    }
}

impl Default for CueEngine {
    fn default() -> Self {
        Self::empty()
    }
}

/// Seal a [`CueRollup`] under the personalization seal keys
/// ([`fauna_client_personalization::model_seal_keys`]) — the exact bytes
/// `PersonalizationClient::model_put` sends for
/// [`fauna_core::scoring::CUES_ROLLUP_FACTOR_V1`] and the nest stores
/// verbatim-opaque. Rides the shared model byte pipeline
/// ([`fauna_client_personalization::seal_model_bytes`]) that the trained-topic
/// model also seals through.
///
/// No plaintext byte cap is applied: the rollup is already bounded to
/// [`CUE_ROLLUP_MAX_ITEMS`] entries, and a full rollup — a few hundred KiB of
/// highly-repetitive post-id JSON — compresses ~5–10× under zstd, staying
/// comfortably below the wire's 512 KiB blob cap. That wire cap is the backstop
/// the manager surfaces if a `put` is ever rejected (owner doc § At rest).
pub fn seal_cue_rollup(rollup: &CueRollup, keys: &ModelSealKey) -> Result<Vec<u8>, ModelSealError> {
    seal_model_bytes(&rollup.to_bytes(), keys)
}

/// Open a sealed cue rollup — the inverse of [`seal_cue_rollup`]. A blob that
/// opens (decrypts + decompresses) but does not decode is
/// [`ModelSealError::Decode`], **never** a silent empty rollup: silently
/// starting fresh would erase every cue the user's *other* devices recorded, on
/// the very next put (the loud-not-empty contract of
/// [`fauna_client_personalization::unseal_model_bytes`], applied to cues). An
/// *absent* blob (no row yet) is the caller's "fresh empty rollup" case and never
/// reaches here.
pub fn unseal_cue_rollup(blob: &[u8], keys: &ModelSealKey) -> Result<CueRollup, ModelSealError> {
    let plaintext = unseal_model_bytes(blob, keys)?;
    CueRollup::from_bytes(&plaintext).ok_or(ModelSealError::Decode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::scoring::cues::{CUE_LONG_DWELL_VISIBLE_PM, CUE_SKIP_VISIBLE_PM};

    /// A verdict a newer build derives — modelled by a twin with one more
    /// variant — rides the sealed rollup into an older build, which decodes
    /// the rollup, carries the string through its own re-seal byte-for-byte,
    /// and never contributes or completes on it (`transport.md` § Schema and
    /// forward-compat discipline → *Rule 3 in full*).
    #[test]
    fn an_unknown_verdict_is_carried_through_the_re_seal() {
        #[derive(Serialize)]
        #[serde(rename_all = "kebab-case")]
        enum NewerVerdict {
            Rewatch,
        }
        #[derive(Serialize)]
        struct NewerItem {
            verdict: NewerVerdict,
            watch_ms_total: u64,
            completions: u32,
            last_at: u64,
        }
        #[derive(Serialize)]
        struct NewerRollup {
            version: u16,
            items: BTreeMap<String, NewerItem>,
        }
        let newer = NewerRollup {
            version: CUE_ROLLUP_VERSION,
            items: [(
                "cat".to_string(),
                NewerItem {
                    verdict: NewerVerdict::Rewatch,
                    watch_ms_total: 40,
                    completions: 0,
                    last_at: 7,
                },
            )]
            .into(),
        };
        let bytes = serde_json::to_vec(&newer).unwrap();
        let mut rollup = CueRollup::from_bytes(&bytes).expect("an older build decodes the rollup");
        let verdict = rollup.verdict("cat").unwrap();
        assert_eq!(verdict, CueVerdict::Other("rewatch".into()));
        assert_eq!(
            verdict.wire_str(),
            None,
            "an unknown verdict is never contributed"
        );
        assert_eq!(
            rollup.to_bytes(),
            bytes,
            "the re-seal keeps it byte-for-byte"
        );

        // Another item's verdict lands beside it without touching it.
        rollup.record("dog", CueVerdict::Skip, 10, 8);
        assert_eq!(
            rollup.verdict("cat"),
            Some(CueVerdict::Other("rewatch".into()))
        );
    }

    /// A non-media exposure with the given ≥long-visibility dwell, seen far from
    /// any prior exposure (never a burst).
    fn text_dwell(id: &str, dwell_long_ms: u64, at: u64) -> CueObservation {
        CueObservation {
            content_id: id.to_string(),
            is_media: false,
            media_played_pm: None,
            // A long-dwell exposure is also skip-visible for at least as long.
            dwell_ms_at_skip_visibility: dwell_long_ms,
            dwell_ms_at_long_visibility: dwell_long_ms,
            observed_at_ms: at,
        }
    }

    /// A media exposure that played to `played_pm` per-mille.
    fn media(id: &str, played_pm: u32, at: u64) -> CueObservation {
        CueObservation {
            content_id: id.to_string(),
            is_media: true,
            media_played_pm: Some(played_pm),
            dwell_ms_at_skip_visibility: 3_000,
            dwell_ms_at_long_visibility: 3_000,
            observed_at_ms: at,
        }
    }

    /// A brief skip-visible glance (`skip_ms` at ≥50% visible), no long dwell.
    fn glance(id: &str, skip_ms: u64, at: u64) -> CueObservation {
        CueObservation {
            content_id: id.to_string(),
            is_media: false,
            media_played_pm: None,
            dwell_ms_at_skip_visibility: skip_ms,
            dwell_ms_at_long_visibility: 0,
            observed_at_ms: at,
        }
    }

    // ---- WatchComplete: the two gates, at their exact boundaries ----

    #[test]
    fn non_media_watch_complete_at_the_dwell_boundary() {
        // 8000ms is complete; one ms under is not.
        assert_eq!(
            CueEngine::derive(&text_dwell("a", CUE_DWELL_LONG_MS, 0), None),
            Some(CueVerdict::WatchComplete)
        );
        assert_eq!(
            CueEngine::derive(&text_dwell("a", CUE_DWELL_LONG_MS - 1, 0), None),
            // just under the long-dwell gate, and well over CUE_SKIP_MS, so
            // neither verdict fires.
            None
        );
    }

    #[test]
    fn media_watch_complete_at_the_playback_boundary() {
        assert_eq!(
            CueEngine::derive(&media("v", CUE_COMPLETE_FRACTION_PM, 0), None),
            Some(CueVerdict::WatchComplete)
        );
        assert_eq!(
            CueEngine::derive(&media("v", CUE_COMPLETE_FRACTION_PM - 1, 0), None),
            // played under 85% and dwelled 3s (> CUE_SKIP_MS) → no verdict.
            None
        );
    }

    #[test]
    fn media_played_none_is_never_complete() {
        let obs = CueObservation {
            content_id: "v".into(),
            is_media: true,
            media_played_pm: None,
            dwell_ms_at_skip_visibility: 500,
            dwell_ms_at_long_visibility: 0,
            observed_at_ms: 0,
        };
        // Un-played media briefly glanced past is a Skip, not a WatchComplete.
        assert_eq!(CueEngine::derive(&obs, None), Some(CueVerdict::Skip));
    }

    // ---- Skip: the brief-glance window + the burst gate ----

    #[test]
    fn skip_only_within_the_brief_window() {
        // Under CUE_SKIP_MS and actually seen → Skip.
        assert_eq!(
            CueEngine::derive(&glance("a", CUE_SKIP_MS - 1, 10_000), None),
            Some(CueVerdict::Skip)
        );
        // Exactly CUE_SKIP_MS is no longer "brief" → no verdict.
        assert_eq!(
            CueEngine::derive(&glance("a", CUE_SKIP_MS, 10_000), None),
            None
        );
        // Never really seen (0 skip-visible ms) → no judgment.
        assert_eq!(CueEngine::derive(&glance("a", 0, 10_000), None), None);
    }

    #[test]
    fn a_fast_scroll_burst_derives_no_skip() {
        // Same brief glance, but the previous exposure was < CUE_BURST_MIN_GAP_MS
        // ago → a flick, not a judgment.
        let prev = Some(10_000u64);
        assert_eq!(
            CueEngine::derive(
                &glance("a", CUE_SKIP_MS - 1, 10_000 + CUE_BURST_MIN_GAP_MS - 1),
                prev
            ),
            None
        );
        // At exactly the gap it counts as normal pace again.
        assert_eq!(
            CueEngine::derive(
                &glance("a", CUE_SKIP_MS - 1, 10_000 + CUE_BURST_MIN_GAP_MS),
                prev
            ),
            Some(CueVerdict::Skip)
        );
    }

    #[test]
    fn watch_complete_is_immune_to_the_burst_gate() {
        // Even hard against the previous exposure, a completed item scores —
        // you cannot complete a video mid-flick, so no suppression is needed.
        let prev = Some(10_000u64);
        assert_eq!(
            CueEngine::derive(&media("v", 1000, 10_000 + 1), prev),
            Some(CueVerdict::WatchComplete)
        );
    }

    // ---- Rollup accumulation: last-wins, completions, watch_ms ----

    #[test]
    fn rewatch_flips_skip_to_watch_complete_last_wins() {
        let mut engine = CueEngine::empty();
        // First: a brief glance → Skip.
        engine.observe(glance("cat", CUE_SKIP_MS - 1, 0));
        assert_eq!(engine.verdict("cat"), Some(CueVerdict::Skip));
        // Later: a full watch of the same item → WatchComplete overwrites.
        engine.observe(media("cat", 1000, 100_000));
        let cues = engine.rollup().get("cat").unwrap();
        assert_eq!(cues.verdict, CueVerdict::WatchComplete);
        assert_eq!(cues.completions, 1);
        assert_eq!(cues.last_at, 100_000);
        // watch_ms accumulated across both exposures (glance + the 3s media dwell).
        assert_eq!(cues.watch_ms_total, (CUE_SKIP_MS - 1) + 3_000);
    }

    #[test]
    fn repeated_completions_accumulate() {
        let mut engine = CueEngine::empty();
        engine.observe(media("v", 900, 0));
        engine.observe(media("v", 950, 100_000));
        let cues = engine.rollup().get("v").unwrap();
        assert_eq!(cues.completions, 2);
        assert_eq!(cues.verdict, CueVerdict::WatchComplete);
    }

    #[test]
    fn a_verdict_less_exposure_records_nothing_but_advances_pace() {
        let mut engine = CueEngine::empty();
        // An ambiguous middle exposure (long enough not to skip, short enough not
        // to complete) records no item…
        let mid = text_dwell("mid", 4_000, 0);
        assert_eq!(engine.observe(mid), None);
        assert!(engine.rollup().is_empty());
        // …but it DID advance the pacing clock: an immediately-following brief
        // glance is now inside the burst window and derives no Skip.
        assert_eq!(
            engine.observe(glance("next", 500, CUE_BURST_MIN_GAP_MS - 1)),
            None
        );
        assert!(engine.rollup().is_empty());
    }

    // ---- Eviction ----

    #[test]
    fn evicts_the_oldest_past_the_cap() {
        let mut rollup = CueRollup::new();
        // Fill to the cap, each item older than the next.
        for i in 0..CUE_ROLLUP_MAX_ITEMS {
            rollup.record(&format!("item-{i:05}"), CueVerdict::Skip, 100, i as u64);
        }
        assert_eq!(rollup.len(), CUE_ROLLUP_MAX_ITEMS);
        assert!(rollup.get("item-00000").is_some());

        // One more, newest of all → the single oldest (item-00000) is evicted.
        rollup.record("newest", CueVerdict::WatchComplete, 100, u64::MAX);
        assert_eq!(rollup.len(), CUE_ROLLUP_MAX_ITEMS);
        assert!(rollup.get("item-00000").is_none(), "oldest not evicted");
        assert!(rollup.get("newest").is_some());
        assert!(rollup.get("item-00001").is_some());
    }

    #[test]
    fn updating_an_existing_item_does_not_grow_or_evict() {
        let mut rollup = CueRollup::new();
        for i in 0..CUE_ROLLUP_MAX_ITEMS {
            rollup.record(&format!("item-{i:05}"), CueVerdict::Skip, 100, i as u64);
        }
        // Re-record an existing item — len stays at the cap, nothing evicted.
        rollup.record("item-00000", CueVerdict::WatchComplete, 100, u64::MAX);
        assert_eq!(rollup.len(), CUE_ROLLUP_MAX_ITEMS);
        assert_eq!(
            rollup.get("item-00000").unwrap().verdict,
            CueVerdict::WatchComplete
        );
    }

    // ---- Serialization round-trip + forward compatibility ----

    #[test]
    fn rollup_bytes_round_trip() {
        let mut rollup = CueRollup::new();
        rollup.record("a", CueVerdict::WatchComplete, 1_234, 5);
        rollup.record("b", CueVerdict::Skip, 800, 6);
        let bytes = rollup.to_bytes();
        let back = CueRollup::from_bytes(&bytes).expect("valid rollup bytes");
        assert_eq!(back, rollup);
    }

    #[test]
    fn to_bytes_is_deterministic() {
        // Reproducible sealed bytes: same logical rollup, byte-identical output,
        // so a no-op re-seal is a no-op put regardless of insertion order.
        let mut a = CueRollup::new();
        a.record("z", CueVerdict::Skip, 1, 1);
        a.record("a", CueVerdict::WatchComplete, 2, 2);
        let mut b = CueRollup::new();
        b.record("a", CueVerdict::WatchComplete, 2, 2);
        b.record("z", CueVerdict::Skip, 1, 1);
        assert_eq!(a.to_bytes(), b.to_bytes());
    }

    #[test]
    fn verdict_strings_are_the_compat_surface() {
        // These exact strings rest in the sealed blob and cross devices.
        let mut rollup = CueRollup::new();
        rollup.record("a", CueVerdict::WatchComplete, 0, 0);
        rollup.record("b", CueVerdict::Skip, 0, 0);
        let json = String::from_utf8(rollup.to_bytes()).unwrap();
        assert!(json.contains("watch-complete"), "got: {json}");
        assert!(json.contains("\"skip\""), "got: {json}");
    }

    #[test]
    fn additive_unknown_fields_are_ignored_on_read() {
        // A future client widens ItemCues; an older client must still read it,
        // dropping only the field it does not know (no data loss on the fields
        // it does).
        let future = br#"{"version":9,"items":{"a":{"verdict":"skip","watch_ms_total":7,"completions":0,"last_at":3,"future_field":42}}}"#;
        let rollup = CueRollup::from_bytes(future).expect("forward-compatible read");
        let cues = rollup.get("a").unwrap();
        assert_eq!(cues.verdict, CueVerdict::Skip);
        assert_eq!(cues.watch_ms_total, 7);
    }

    #[test]
    fn garbage_bytes_are_rejected_not_silently_fresh() {
        // A corrupt/unopenable blob must be None (a surfaced error), never an
        // empty rollup that would erase the user's other devices' cues on put.
        assert!(CueRollup::from_bytes(b"not json at all").is_none());
        assert!(CueRollup::from_bytes(b"").is_none());
    }

    #[test]
    fn visibility_fraction_constants_bracket_the_gates() {
        // A guard that the two visibility thresholds stay ordered (skip ≤ long),
        // so the tracker's two dwell buckets are nested as the derivation assumes.
        const { assert!(CUE_SKIP_VISIBLE_PM <= CUE_LONG_DWELL_VISIBLE_PM) };
    }

    // ---- Seal round-trip (owner doc § At rest) ----

    use fauna_client_personalization::model_seal_keys;
    use fauna_core::crypto::{BackupKey, DelegableKindKeys};

    fn seal_key() -> DelegableKindKeys {
        model_seal_keys(&BackupKey::derive(&[3u8; 32]))
    }

    fn a_rollup() -> CueRollup {
        let mut r = CueRollup::new();
        r.record("cat-video", CueVerdict::WatchComplete, 12_000, 100);
        r.record("fish-post", CueVerdict::Skip, 800, 200);
        r
    }

    #[test]
    fn seal_then_unseal_round_trips_the_rollup() {
        let r = a_rollup();
        let blob = seal_cue_rollup(&r, &seal_key()).unwrap();
        assert_eq!(unseal_cue_rollup(&blob, &seal_key()).unwrap(), r);
    }

    #[test]
    fn the_sealed_rollup_does_not_leak_content_ids() {
        // The seal is a privacy boundary: the nest must not learn which posts the
        // user watched or skipped.
        let blob = seal_cue_rollup(&a_rollup(), &seal_key()).unwrap();
        let haystack = String::from_utf8_lossy(&blob);
        for id in ["cat-video", "fish-post"] {
            assert!(
                !haystack.contains(id),
                "sealed blob leaked content id {id:?}"
            );
        }
    }

    #[test]
    fn a_second_device_with_the_same_seed_opens_the_rollup() {
        // Every device derives the same seal keys from the identity seed → the
        // phone's watches inform the desktop's feeds (owner doc § Why on the nest).
        let seed = [42u8; 32];
        let blob =
            seal_cue_rollup(&a_rollup(), &model_seal_keys(&BackupKey::derive(&seed))).unwrap();
        assert_eq!(
            unseal_cue_rollup(&blob, &model_seal_keys(&BackupKey::derive(&seed))).unwrap(),
            a_rollup()
        );
    }

    #[test]
    fn a_wrong_key_fails_loudly_rather_than_reading_as_empty() {
        let blob = seal_cue_rollup(&a_rollup(), &seal_key()).unwrap();
        let wrong = model_seal_keys(&BackupKey::derive(&[9u8; 32]));
        // Not an empty rollup — an unopenable blob must never be mistaken for
        // "no cues yet", which would erase the user's other devices on next put.
        assert!(matches!(
            unseal_cue_rollup(&blob, &wrong),
            Err(ModelSealError::Decrypt(_))
        ));
    }
}
