//! The shared client-side **local-detection store** + the moderation-queue
//! **union/dedupe** helper — the encrypted-mode social-content moderation signal
//! the nest cannot produce.
//!
//! # Why this exists
//!
//! The moderation queue is the **union of two sources** (ratified 2026-07-02,
//! `docs/goal/behavior/moderation.md` § Layout & flow + § State & data shape):
//!
//! 1. the nest's server-issued [`ObligationAction`]s from `fauna.moderation.actions`
//!    (the *enforcement* half — mail-ingest / admin quarantine·reject·label + appeals,
//!    plus plaintext-mode social labels), and
//! 2. the client's own **post-decrypt local detections** — category + confidence,
//!    **no** action.
//!
//! In **encrypted mode** (the privacy-respecting default) the nest holds only
//! ciphertext, so it runs *no* content scorer (`content-scoring.md` § "The
//! encrypted-mode nest never runs a content scorer") and `fauna.moderation.actions`
//! carries only mail-ingest / admin actions — never social-content labels. The
//! *only* social-content moderation signal there is the one the client computes
//! **post-decrypt**, where it legitimately holds plaintext (`content-scoring.md`
//! § "The two plaintext positions in encrypted mode" → position 2, the user's
//! client). This store retains those detections so the queue is non-empty and
//! train-correctable in encrypted mode.
//!
//! # Shape: the shared twin of web's `flaggedItems`
//!
//! This lifts web's `apps/fauna-web/src/lib/moderation.ts` `flaggedItems` store
//! (an in-memory Svelte writable) into shared Rust so **all seven apps** consume
//! one shape (priority #1/#2) rather than each hand-rolling a per-platform list.
//! It mirrors that store's semantics — spam-only, a confidence gate, a bounded
//! most-recent-first ring, and remove-on-train — with three deliberate upgrades
//! for the shared home:
//!
//! - **Wire-aligned encoding.** Confidence rides as per-mille `u16` (0–1000) and
//!   the timestamp as `i64`, matching [`ObligationAction`] exactly, so
//!   [`merge_queue`] unions the two sources without a units conversion (web's TS
//!   store carried `0.0–1.0` floats + millisecond numbers — fine in JS, but the
//!   dag-cbor wire forbids floats and the queue rows it merges against are
//!   per-mille).
//! - **Dedupe on observe.** Re-observing a `content_id` (a message re-decrypted on
//!   a later drain pass) **replaces** the prior detection rather than appending a
//!   duplicate — web's store slices to a cap without deduping, which can show the
//!   same item twice.
//! - **Not a global.** This is a plain struct a session owns (mirroring
//!   `fauna_client_mail_settings::InboxSpamScorer`, held per-connection by
//!   `NestMailInboundSource`), not a process-global module singleton like web's —
//!   the multi-user-correct shape.
//!
//! # Placement (not re-litigated here)
//!
//! The classifier that *produces* the labels fed to [`LocalDetectionStore::observe`]
//! is the shared `fauna_core::text_heuristic::classify_text` (+ the per-user
//! Bayesian scorer), run by the caller at its post-decrypt hook — governed
//! by `content-scoring.md`, not here. This module only **retains** the results and
//! **merges** them with the server queue; it runs no classifier and reads no
//! content.

use fauna_protocol::moderation::ObligationAction;

/// The most-recent local detections retained for the moderation UI — matches web's
/// `MAX_FLAGGED` (`apps/fauna-web/src/lib/moderation.ts`). A bounded ring keeps the
/// queue view cheap and recent; older detections age out.
pub const MAX_LOCAL_DETECTIONS: usize = 50;

/// A label is retained as a local detection only when its confidence exceeds this
/// per-mille gate — the wire-encoded twin of web's `confidence > 0.3` filter
/// (`0.3 * 1000 = 300`). Below it, a heuristic hit is too weak to surface for
/// review.
pub const SPAM_FLAG_THRESHOLD_PER_MILLE: u16 = 300;

/// The one category surfaced as a local detection today: `spam`. Ratified
/// spam-only (`moderation.md` § State & data shape — "spam-only `{ category,
/// confidence }`"), matching web's `flaggedItems` filter. `phishing` (also emitted
/// by `classify_text`) and the other canonical categories are **reserved** — the
/// server queue surfaces them via [`ObligationAction`]; widening the *local*
/// signal is a product decision (extend this + the doc), not a silent change.
pub const LOCAL_DETECTION_CATEGORY: &str = "spam";

/// One classifier output fed to [`LocalDetectionStore::observe`] — a `(category,
/// confidence)` pair, decoupled from the concrete classifier crate so this store
/// stays dependency-free. The caller (the post-decrypt hook) maps its classifier's
/// labels (e.g. `fauna_client_core::scan::ClassificationResult`, confidence `0.0–1.0`)
/// to this per-mille form.
#[derive(Debug, Clone, PartialEq)]
pub struct DetectionLabel {
    /// One of the canonical 5 categories (`moderation.md` § Categories). Only
    /// [`LOCAL_DETECTION_CATEGORY`] is retained today.
    pub category: String,
    /// Classifier confidence, per-mille (0–1000).
    pub confidence_per_mille: u16,
}

/// A retained client-side post-decrypt detection — the encrypted-mode
/// social-content moderation signal. Carries **no** enforcement `action` (that is
/// the server queue's half); its queue row shows a blank action column.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct LocalDetection {
    /// The classified content's id (a conversation channel / post id — the caller's
    /// content ref, the same key [`ObligationAction::content_id`] uses so the two
    /// sources dedupe).
    pub content_id: String,
    /// The content kind (`"post"`, `"message"`, …) for the row's content ref; may
    /// be empty when the hook has no kind to attribute.
    pub content_type: String,
    /// The detected category — [`LOCAL_DETECTION_CATEGORY`] today.
    pub category: String,
    /// Confidence per-mille (0–1000), matching [`ObligationAction::confidence_per_mille`].
    pub confidence_per_mille: u16,
    /// Detection time, same unit as [`ObligationAction::timestamp`] (microsecond
    /// epoch), supplied by the caller.
    pub timestamp: i64,
}

/// A bounded, most-recent-first store of post-decrypt local detections — the shared
/// twin of web's `flaggedItems`. A session owns one (per authenticated connection);
/// the receive loop [`observe`](Self::observe)s each just-decrypted message, the
/// queue view reads [`snapshot`](Self::snapshot), and a train correction
/// [`remove`](Self::remove)s the corrected row.
#[derive(Debug)]
pub struct LocalDetectionStore {
    /// Newest-first; length ≤ `cap`. Deduped by `content_id`.
    detections: Vec<LocalDetection>,
    cap: usize,
}

impl Default for LocalDetectionStore {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalDetectionStore {
    /// A store bounded to [`MAX_LOCAL_DETECTIONS`].
    pub fn new() -> Self {
        Self::with_cap(MAX_LOCAL_DETECTIONS)
    }

    /// A store bounded to `cap` most-recent detections (`0` disables retention —
    /// [`observe`](Self::observe) then no-ops).
    pub fn with_cap(cap: usize) -> Self {
        Self {
            detections: Vec::new(),
            cap,
        }
    }

    /// Retain the qualifying labels from one just-decrypted item's classification.
    ///
    /// A label is retained iff its `category` is [`LOCAL_DETECTION_CATEGORY`] and
    /// its `confidence_per_mille` **exceeds** [`SPAM_FLAG_THRESHOLD_PER_MILLE`] —
    /// the wire-encoded twin of web's `category === 'spam' && confidence > 0.3`
    /// filter (`moderation.ts`). The highest-confidence qualifying label for the
    /// item becomes its detection (an item is one row, not one-per-label).
    ///
    /// Deduped by `content_id`: re-observing an item **replaces** its prior
    /// detection and moves it to the front (newest-first), so a message re-decrypted
    /// on a later drain pass never appears twice. The store is then truncated to its
    /// cap. A `cap` of `0`, or no qualifying label, leaves the store unchanged.
    pub fn observe(
        &mut self,
        content_id: impl Into<String>,
        content_type: impl Into<String>,
        timestamp: i64,
        labels: &[DetectionLabel],
    ) {
        if self.cap == 0 {
            return;
        }
        // The item's strongest qualifying label, if any.
        let Some(best) = labels
            .iter()
            .filter(|l| {
                l.category == LOCAL_DETECTION_CATEGORY
                    && l.confidence_per_mille > SPAM_FLAG_THRESHOLD_PER_MILLE
            })
            .max_by_key(|l| l.confidence_per_mille)
        else {
            return;
        };
        let content_id = content_id.into();
        // Dedupe: drop any prior detection for this content_id (replace, not append).
        self.detections.retain(|d| d.content_id != content_id);
        self.detections.insert(
            0,
            LocalDetection {
                content_id,
                content_type: content_type.into(),
                category: best.category.clone(),
                confidence_per_mille: best.confidence_per_mille,
                timestamp,
            },
        );
        self.detections.truncate(self.cap);
    }

    /// A clone of the retained detections, newest-first — the queue view's read.
    pub fn snapshot(&self) -> Vec<LocalDetection> {
        self.detections.clone()
    }

    /// Borrow the retained detections, newest-first (allocation-free read).
    pub fn detections(&self) -> &[LocalDetection] {
        &self.detections
    }

    /// Drop the detection for `content_id` after the user trains a correction on it
    /// (web's `removeFlagged`). No-op if absent. Returns `true` iff one was removed.
    pub fn remove(&mut self, content_id: &str) -> bool {
        let before = self.detections.len();
        self.detections.retain(|d| d.content_id != content_id);
        self.detections.len() != before
    }

    /// Retained-detection count.
    pub fn len(&self) -> usize {
        self.detections.len()
    }

    /// True when nothing is retained.
    pub fn is_empty(&self) -> bool {
        self.detections.is_empty()
    }
}

/// Which source a [`QueueRow`] came from — the server obligation queue or a local
/// post-decrypt detection. The UI branches on it only for the action column (server
/// rows carry an enforcement `action`; local rows are blank).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum QueueRowSource {
    /// A nest-issued [`ObligationAction`] (mail-ingest / admin enforcement, appeals,
    /// or plaintext-mode social labels).
    Server,
    /// A client-side post-decrypt [`LocalDetection`] — blank action column.
    Local,
}

/// One unified moderation-queue row — the merge of the two sources. Uniform across
/// server and local rows so a client's row renderer branches only on `action`
/// being `Some`/`None` (never on which list a row came from).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct QueueRow {
    pub content_id: String,
    pub content_type: String,
    pub category: String,
    pub confidence_per_mille: u16,
    /// The enforcement action discriminant for a server row; **`None`** for a local
    /// detection (blank action column — never fabricate one, `moderation.md` § Don't
    /// do these).
    pub action: Option<u8>,
    pub timestamp: i64,
    pub source: QueueRowSource,
}

impl QueueRow {
    fn from_server(a: &ObligationAction) -> Self {
        Self {
            content_id: a.content_id.clone(),
            content_type: a.content_type.clone(),
            category: a.category.clone(),
            confidence_per_mille: a.confidence_per_mille,
            action: Some(a.action),
            timestamp: a.timestamp,
            source: QueueRowSource::Server,
        }
    }

    fn from_local(d: &LocalDetection) -> Self {
        Self {
            content_id: d.content_id.clone(),
            content_type: d.content_type.clone(),
            category: d.category.clone(),
            confidence_per_mille: d.confidence_per_mille,
            action: None,
            timestamp: d.timestamp,
            source: QueueRowSource::Local,
        }
    }
}

/// Merge the server obligation queue **∪** the client's local detections into one
/// newest-first queue, **deduped by `content_id`** (ratified — `moderation.md`
/// § Layout & flow: "server and local rows deduplicate by `content_id`").
///
/// On a `content_id` collision the **server row wins** — it carries the enforcement
/// `action` and appeal affordance the local detection lacks Rows are sorted by
/// `timestamp` descending (ties broken by `content_id` for a stable order).
///
/// This is the one shared union used by every app's moderation-queue VM (web via
/// WASM, natives via UniFFI), so the dedupe rule can never drift between clients.
pub fn merge_queue(server: &[ObligationAction], local: &[LocalDetection]) -> Vec<QueueRow> {
    let mut rows: Vec<QueueRow> = Vec::with_capacity(server.len() + local.len());
    // Server rows first — they own their content_id.
    for a in server {
        rows.push(QueueRow::from_server(a));
    }
    // Local detections only for content_ids the server queue doesn't already cover.
    for d in local {
        let covered = server.iter().any(|a| a.content_id == d.content_id);
        if !covered {
            rows.push(QueueRow::from_local(d));
        }
    }
    rows.sort_by(|a, b| {
        b.timestamp
            .cmp(&a.timestamp)
            .then_with(|| a.content_id.cmp(&b.content_id))
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(category: &str, conf: u16) -> DetectionLabel {
        DetectionLabel {
            category: category.into(),
            confidence_per_mille: conf,
        }
    }

    fn action(content_id: &str, action: u8, ts: i64) -> ObligationAction {
        ObligationAction {
            id: 1,
            content_type: "post".into(),
            content_id: content_id.into(),
            category: "spam".into(),
            confidence_per_mille: 900,
            action,
            timestamp: ts,
            extra: Default::default(),
        }
    }

    // ── LocalDetectionStore::observe ────────────────────────────────────────

    #[test]
    fn observe_retains_a_spam_label_above_threshold() {
        let mut s = LocalDetectionStore::new();
        s.observe("c1", "message", 100, &[label("spam", 800)]);
        assert_eq!(s.len(), 1);
        let d = &s.detections()[0];
        assert_eq!(d.content_id, "c1");
        assert_eq!(d.content_type, "message");
        assert_eq!(d.category, "spam");
        assert_eq!(d.confidence_per_mille, 800);
        assert_eq!(d.timestamp, 100);
    }

    #[test]
    fn observe_drops_below_threshold_and_non_spam() {
        let mut s = LocalDetectionStore::new();
        // At the threshold (not above) → dropped, mirroring web's `> 0.3`.
        s.observe(
            "c1",
            "message",
            1,
            &[label("spam", SPAM_FLAG_THRESHOLD_PER_MILLE)],
        );
        // Phishing above threshold → dropped (spam-only local signal).
        s.observe("c2", "message", 2, &[label("phishing", 999)]);
        assert!(s.is_empty());
    }

    #[test]
    fn observe_keeps_only_the_strongest_qualifying_label_as_one_row() {
        let mut s = LocalDetectionStore::new();
        s.observe(
            "c1",
            "message",
            1,
            &[
                label("spam", 400),
                label("phishing", 999),
                label("spam", 700),
            ],
        );
        assert_eq!(s.len(), 1);
        assert_eq!(s.detections()[0].confidence_per_mille, 700);
    }

    #[test]
    fn observe_dedupes_by_content_id_replacing_and_promoting() {
        let mut s = LocalDetectionStore::new();
        s.observe("c1", "message", 1, &[label("spam", 400)]);
        s.observe("c2", "message", 2, &[label("spam", 500)]);
        // Re-observe c1 with a new score + newer ts → replaces, moves to front.
        s.observe("c1", "message", 3, &[label("spam", 950)]);
        assert_eq!(s.len(), 2);
        assert_eq!(s.detections()[0].content_id, "c1");
        assert_eq!(s.detections()[0].confidence_per_mille, 950);
        assert_eq!(s.detections()[0].timestamp, 3);
        assert_eq!(s.detections()[1].content_id, "c2");
    }

    #[test]
    fn observe_bounds_to_cap_newest_first() {
        let mut s = LocalDetectionStore::with_cap(2);
        s.observe("c1", "m", 1, &[label("spam", 800)]);
        s.observe("c2", "m", 2, &[label("spam", 800)]);
        s.observe("c3", "m", 3, &[label("spam", 800)]);
        assert_eq!(s.len(), 2);
        // Newest two kept, newest first; c1 aged out.
        assert_eq!(s.detections()[0].content_id, "c3");
        assert_eq!(s.detections()[1].content_id, "c2");
    }

    #[test]
    fn observe_cap_zero_disables_retention() {
        let mut s = LocalDetectionStore::with_cap(0);
        s.observe("c1", "m", 1, &[label("spam", 999)]);
        assert!(s.is_empty());
    }

    #[test]
    fn observe_no_qualifying_label_is_noop() {
        let mut s = LocalDetectionStore::new();
        s.observe("c1", "m", 1, &[]);
        s.observe("c2", "m", 2, &[label("spam", 10)]);
        assert!(s.is_empty());
    }

    #[test]
    fn remove_drops_the_row_after_training() {
        let mut s = LocalDetectionStore::new();
        s.observe("c1", "m", 1, &[label("spam", 800)]);
        s.observe("c2", "m", 2, &[label("spam", 800)]);
        assert!(s.remove("c1"));
        assert!(!s.remove("c1")); // idempotent
        assert_eq!(s.len(), 1);
        assert_eq!(s.detections()[0].content_id, "c2");
    }

    // ── merge_queue ─────────────────────────────────────────────────────────

    fn local(content_id: &str, conf: u16, ts: i64) -> LocalDetection {
        LocalDetection {
            content_id: content_id.into(),
            content_type: "message".into(),
            category: "spam".into(),
            confidence_per_mille: conf,
            timestamp: ts,
        }
    }

    #[test]
    fn merge_empty_is_empty() {
        assert!(merge_queue(&[], &[]).is_empty());
    }

    #[test]
    fn merge_server_only_carries_action() {
        let rows = merge_queue(&[action("c1", 2, 100)], &[]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source, QueueRowSource::Server);
        assert_eq!(rows[0].action, Some(2));
    }

    #[test]
    fn merge_local_only_has_blank_action() {
        let rows = merge_queue(&[], &[local("c1", 800, 100)]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source, QueueRowSource::Local);
        assert_eq!(rows[0].action, None);
        assert_eq!(rows[0].confidence_per_mille, 800);
    }

    #[test]
    fn merge_dedupes_by_content_id_server_wins() {
        // Same content_id in both → one row, the server's (with action).
        let rows = merge_queue(&[action("dup", 3, 100)], &[local("dup", 800, 200)]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source, QueueRowSource::Server);
        assert_eq!(rows[0].action, Some(3));
    }

    #[test]
    fn merge_unions_distinct_ids_newest_first() {
        let rows = merge_queue(
            &[action("s1", 1, 100), action("s2", 2, 400)],
            &[local("l1", 800, 300), local("l2", 700, 200)],
        );
        assert_eq!(rows.len(), 4);
        // Sorted by timestamp desc: s2(400), l1(300), l2(200), s1(100).
        assert_eq!(rows[0].content_id, "s2");
        assert_eq!(rows[1].content_id, "l1");
        assert_eq!(rows[2].content_id, "l2");
        assert_eq!(rows[3].content_id, "s1");
        assert_eq!(rows[1].action, None);
        assert_eq!(rows[0].action, Some(2));
    }

    #[test]
    fn merge_ties_break_stably_by_content_id() {
        let rows = merge_queue(&[action("b", 1, 100), action("a", 1, 100)], &[]);
        assert_eq!(rows[0].content_id, "a");
        assert_eq!(rows[1].content_id, "b");
    }

    // ── serde wire shape (the web `moderationQueue` binding consumes this) ─────
    //
    // `fauna-wasm`'s `moderationQueue` deserializes the `local` input and
    // serializes the merged `QueueRow`s for the web SPA via serde_wasm_bindgen,
    // which uses these exact serde field names / enum tags. Pin them so a rename
    // here can't silently break web's row renderer (the UniFFI natives are already
    // pinned by the generated bindings).

    #[test]
    fn queue_row_serde_shape_is_stable() {
        let server = QueueRow::from_server(&action("c1", 2, 100));
        let json = serde_json::to_value(&server).unwrap();
        assert_eq!(json["content_id"], "c1");
        assert_eq!(json["content_type"], "post");
        assert_eq!(json["category"], "spam");
        assert_eq!(json["confidence_per_mille"], 900);
        assert_eq!(json["action"], 2);
        assert_eq!(json["timestamp"], 100);
        assert_eq!(json["source"], "Server");

        let local_row = QueueRow::from_local(&local("c2", 800, 200));
        let json = serde_json::to_value(&local_row).unwrap();
        assert_eq!(json["content_id"], "c2");
        // A local detection's action column is genuinely blank — never fabricated
        // (`moderation.md` § Don't do these).
        assert!(json["action"].is_null());
        assert_eq!(json["source"], "Local");
    }

    #[test]
    fn local_detection_deserializes_from_wire_shape() {
        // Web passes its post-decrypt detections to `moderationQueue` in the shared
        // per-mille / microsecond `LocalDetection` shape (not web's legacy `0..1`
        // float `flaggedItems`) — pin the field names it must produce.
        let d: LocalDetection = serde_json::from_value(serde_json::json!({
            "content_id": "abc",
            "content_type": "message",
            "category": "spam",
            "confidence_per_mille": 640,
            "timestamp": 1_700_000_000_000_000i64,
        }))
        .unwrap();
        assert_eq!(d.content_id, "abc");
        assert_eq!(d.confidence_per_mille, 640);
        assert_eq!(d.timestamp, 1_700_000_000_000_000);
    }
}
