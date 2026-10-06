//! Stats WS-RPC payload types. A behavior-preserving transport migration of the
//! (now-deleted) bearer-authed HTTP route `GET /api/v1/stats`
//! onto the per-actor WS-RPC
//! connection — Track B17 of the WS-RPC-everywhere migration
//! (tracked internally). The handler logic reuses the
//! existing `backup::stats::{compute_global_stats, compute_folder_stats}`
//! computation exactly (no shared core — the handler is one compute call plus
//! reply shaping), mirroring `account_handlers` / `pending_action_handlers`.
//!
//! One kind (api-layers.md § Stats, "Migrating to WS-RPC: `stats.get`"):
//!
//! - `fauna.stats.get` ≡ GET `/api/v1/stats` — repository storage stats. The
//!   HTTP twin dispatches on a `?folder=` query param onto one of two reply
//!   shapes (global vs per-folder), so the kind keeps that single-endpoint
//!   shape: [`StatsGetRequest`] carries `folder: Option<String>`, and
//!   [`StatsGetReply`] is the internally-tagged enum of the two shapes
//!   (`global` / `folder`).
//!
//! These ride the **bearer** connection (the twin used plain `BearerAuth`), but
//! the stats are global / per-folder — **not** actor-scoped — so the handler
//! ignores the connection `actor_id` for data selection (it is used only for the
//! `User | Admin` allowlist check, preserving the twin's "any authenticated
//! actor" reach).
//!
//! Wire convention (matching `account.rs` / `pending_actions.rs`): the dag-cbor
//! wire forbids floats, so the twin's `dedup_ratio: f64`
//! (`backup/stats.rs:14,32`) rides as `dedup_ratio_micro: i64`
//! (= ratio × 1e6, rounded) — the feed-`score` micro-units precedent
//! (api-layers.md:154). The scale is self-documented in the field name. Every
//! other numeric field is `i64`; optionals are plain `Option` (no
//! `Option<Option>`). The discriminated reply follows the established
//! internally-tagged-enum convention (`bridge_routing::ResolveRecipientReply`);
//! per that precedent the tagged-enum *variants* carry no `extra` forward-compat
//! map — the request and the plain [`BlobTypeCounts`] sub-struct do.
//!
//! Kind registry: `kind.rs::register_stats_kinds`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::{ByteBuf, Value};

/// Multiplier converting the twin's `dedup_ratio: f64` to the wire's
/// `dedup_ratio_micro: i64`. `ratio × 1e6`, rounded. Inverse on the client:
/// divide by `1_000_000`.
pub const DEDUP_RATIO_MICRO_SCALE: f64 = 1_000_000.0;

// ── fauna.stats.get (≡ GET /api/v1/stats[?folder=…]) ──────────────────────

/// Request repository stats. `folder = None` → global stats; `Some(name)` →
/// the named folder's stats (the twin's `?folder=` query param).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct StatsGetRequest {
    /// Folder name to scope to; `None` → global stats.
    pub folder: Option<String>,
    /// Hash-first addressing (S5b) — see `crate::folders::FolderUpdateRequest::name_hash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Blob-type breakdown in the global reply (the twin's `blob_types`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BlobTypeCounts {
    pub chunk: i64,
    pub manifest: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Repository stats reply — one of two shapes, selected by the request's
/// `folder`. Internally tagged on `scope` (`global` / `folder`), the
/// established discriminated-reply convention
/// (`bridge_routing::ResolveRecipientReply`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum StatsGetReply {
    /// `folder` omitted → nest-wide totals (the twin's `GlobalStats`).
    Global {
        total_size_bytes: i64,
        total_blobs: i64,
        total_snapshots: i64,
        total_folders: i64,
        /// Deduplication ratio × 1e6 (the twin's `dedup_ratio: f64`; floats are
        /// forbidden on the dag-cbor wire). Divide by 1e6 client-side.
        dedup_ratio_micro: i64,
        blob_types: BlobTypeCounts,
    },
    /// `folder` present → that folder's stats (the twin's `FolderStats`).
    Folder {
        folder: String,
        snapshot_count: i64,
        /// `created_at` of the latest snapshot; `None` when the set has none.
        latest_snapshot: Option<i64>,
        total_files: i64,
        raw_size_bytes: i64,
        stored_size_bytes: i64,
        /// Deduplication ratio × 1e6 (see `Global::dedup_ratio_micro`).
        dedup_ratio_micro: i64,
        /// `"s3"` if any destination is S3, else `"local"`.
        storage_backend: String,
    },
    /// A scope a newer build added that this build cannot read
    /// (`transport.md` § Schema and forward-compat discipline, rule 3: open,
    /// collapsing). It renders as stats unavailable. Never serialized: a path that would re-emit it
    /// fails instead of replacing the newer value.
    #[serde(other, skip_serializing)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::test_support::assert_round_trips;
    use crate::codec::{decode_strict as decode, encode_canonical};

    #[test]
    fn request_round_trips_global_and_folder() {
        // Global: folder None → null on the wire → round-trips back to None.
        assert_round_trips(&StatsGetRequest {
            folder: None,
            ..Default::default()
        });
        let decoded: StatsGetRequest = decode(
            &encode_canonical(&StatsGetRequest {
                folder: None,
                ..Default::default()
            })
            .unwrap(),
        )
        .unwrap();
        assert!(decoded.folder.is_none());

        // Folder scoped.
        assert_round_trips(&StatsGetRequest {
            folder: Some("photos".into()),
            ..Default::default()
        });
    }

    #[test]
    fn global_reply_round_trips() {
        assert_round_trips(&StatsGetReply::Global {
            total_size_bytes: 1_234_567,
            total_blobs: 42,
            total_snapshots: 7,
            total_folders: 3,
            dedup_ratio_micro: 3_500_000, // ratio 3.5
            blob_types: BlobTypeCounts {
                chunk: 40,
                manifest: 2,
                extra: BTreeMap::new(),
            },
        });
        // Empty repo: ratio 1.0 → 1_000_000.
        assert_round_trips(&StatsGetReply::Global {
            total_size_bytes: 0,
            total_blobs: 0,
            total_snapshots: 0,
            total_folders: 0,
            dedup_ratio_micro: 1_000_000,
            blob_types: BlobTypeCounts {
                chunk: 0,
                manifest: 0,
                extra: BTreeMap::new(),
            },
        });
    }

    #[test]
    fn folder_reply_round_trips_with_and_without_latest() {
        // With a latest snapshot.
        assert_round_trips(&StatsGetReply::Folder {
            folder: "photos".into(),
            snapshot_count: 5,
            latest_snapshot: Some(1_700_000_000),
            total_files: 120,
            raw_size_bytes: 9_000_000,
            stored_size_bytes: 3_000_000,
            dedup_ratio_micro: 3_000_000, // ratio 3.0
            storage_backend: "s3".into(),
        });
        // No snapshots → latest_snapshot None → null → round-trips to None.
        let bare = StatsGetReply::Folder {
            folder: "empty".into(),
            snapshot_count: 0,
            latest_snapshot: None,
            total_files: 0,
            raw_size_bytes: 0,
            stored_size_bytes: 0,
            dedup_ratio_micro: 1_000_000,
            storage_backend: "local".into(),
        };
        let decoded: StatsGetReply = decode(&encode_canonical(&bare).unwrap()).unwrap();
        assert_eq!(bare, decoded);
        match decoded {
            StatsGetReply::Folder {
                latest_snapshot, ..
            } => assert!(latest_snapshot.is_none()),
            _ => panic!("expected Folder variant"),
        }
    }

    #[test]
    fn reply_variants_are_tag_discriminated() {
        // The `scope` tag distinguishes the two shapes on the wire: decoding the
        // encoded bytes into a generic `Value` shows a `scope` map key.
        let bytes = encode_canonical(&StatsGetReply::Global {
            total_size_bytes: 1,
            total_blobs: 1,
            total_snapshots: 0,
            total_folders: 0,
            dedup_ratio_micro: 1_000_000,
            blob_types: BlobTypeCounts::default(),
        })
        .unwrap();
        let value: Value = decode(&bytes).unwrap();
        let scope = match &value {
            Value::Map(entries) => match entries.get("scope") {
                Some(Value::String(s)) => Some(s.clone()),
                _ => None,
            },
            _ => None,
        };
        assert_eq!(scope.as_deref(), Some("global"));
        // And it round-trips back to the Global variant.
        let decoded: StatsGetReply = decode(&bytes).unwrap();
        assert!(matches!(decoded, StatsGetReply::Global { .. }));
    }
}
