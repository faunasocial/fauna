//! Content-label WS-RPC payload types. A behavior-preserving transport
//! migration of the (now-deleted) HTTP routes `POST /api/v1/labels` +
//! `GET /api/v1/labels/{content_id}` onto the per-actor WS-RPC connection — hub Track B8 of
//! the WS-RPC-everywhere migration (tracked internally). The handler logic reuses the
//! existing `CacheDb` label methods exactly (no shared core), mirroring the
//! account / pending-actions surfaces.
//!
//! Two kinds:
//!
//! - `fauna.labels.attach` ≡ POST `/api/v1/labels` — attach one or more
//!   moderation labels (category + confidence) to a content item.
//! - `fauna.labels.list` ≡ GET `/api/v1/labels/{content_id}` — read all labels
//!   on a content item, highest-confidence first.
//!
//! Wire convention (matching `account.rs` / `pending_actions.rs`): the dag-cbor
//! wire **forbids floats**, so the twin's `confidence: f64` (0.0–1.0) rides as
//! `confidence_per_mille: i64` (0–1000) — the established scaling (FilterRule
//! per-mille `u16`, feed-`score` micro-`i64`, spam thresholds). The handler maps
//! per-mille ↔ f64 and rejects out-of-range per-mille with
//! `fauna.labels.invalid_request`, mirroring the twin's 0.0–1.0 reject. Every
//! optional is a plain `Option` (no `Option<Option>`).
//!
//! Kind registry: `kind.rs::register_labels_kinds`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::Value;

// ── fauna.labels.attach (≡ POST /api/v1/labels) ─────────────────────────────

/// One label to attach. `confidence_per_mille` is the 0–1000 scaling of the
/// twin's 0.0–1.0 `confidence` (the dag-cbor float ban). `source` defaults to
/// `"api"` when absent (the twin's `default_source`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LabelInput {
    pub category: String,
    pub confidence_per_mille: i64,
    pub source: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Attach labels to a content item. `content_id` is the content identifier the
/// twin took in the body (content type is fixed to `"post"`, as the twin
/// hard-coded). Non-empty, max 50 labels.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LabelsAttachRequest {
    pub content_id: String,
    pub labels: Vec<LabelInput>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `{ stored }` — the count of labels written (the twin's body).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LabelsAttachReply {
    pub stored: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.labels.list (≡ GET /api/v1/labels/{content_id}) ───────────────────

/// List labels on a content item.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LabelsListRequest {
    pub content_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One stored label — the projection the twin emitted (`mechanism_type` is the
/// `u8` mechanism tag, here `i64` per the all-numeric-`i64` wire convention).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LabelOutput {
    pub category: String,
    pub confidence_per_mille: i64,
    pub mechanism_type: i64,
    pub created_at: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The list reply — labels for the queried content item, highest-confidence
/// first (the twin's `ORDER BY confidence DESC`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LabelsListReply {
    pub content_id: String,
    pub labels: Vec<LabelOutput>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::test_support::assert_round_trips;
    use crate::codec::{decode_strict as decode, encode_canonical};

    #[test]
    fn attach_request_round_trip_with_and_without_source() {
        assert_round_trips(&LabelsAttachRequest {
            content_id: "post-1".into(),
            labels: vec![
                LabelInput {
                    category: "spam".into(),
                    confidence_per_mille: 900,
                    source: Some("classifier-v2".into()),
                    extra: BTreeMap::new(),
                },
                LabelInput {
                    category: "nsfw".into(),
                    confidence_per_mille: 1000,
                    source: None,
                    extra: BTreeMap::new(),
                },
            ],
            extra: BTreeMap::new(),
        });
        // source None → null → round-trips back to None (plain Option).
        let no_source = LabelInput {
            category: "x".into(),
            confidence_per_mille: 0,
            source: None,
            extra: BTreeMap::new(),
        };
        let decoded: LabelInput = decode(&encode_canonical(&no_source).unwrap()).unwrap();
        assert_eq!(no_source, decoded);
        assert!(decoded.source.is_none());
    }

    #[test]
    fn attach_reply_round_trip() {
        assert_round_trips(&LabelsAttachReply {
            stored: 2,
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn list_request_and_reply_round_trip() {
        assert_round_trips(&LabelsListRequest {
            content_id: "post-1".into(),
            extra: BTreeMap::new(),
        });
        assert_round_trips(&LabelsListReply {
            content_id: "post-1".into(),
            labels: vec![LabelOutput {
                category: "spam".into(),
                confidence_per_mille: 850,
                mechanism_type: 0,
                created_at: 1_700_000_000,
                extra: BTreeMap::new(),
            }],
            extra: BTreeMap::new(),
        });
        // Empty list round-trips to an empty Vec.
        let empty = LabelsListReply {
            content_id: "post-1".into(),
            labels: vec![],
            extra: BTreeMap::new(),
        };
        let decoded: LabelsListReply = decode(&encode_canonical(&empty).unwrap()).unwrap();
        assert!(decoded.labels.is_empty());
    }
}
