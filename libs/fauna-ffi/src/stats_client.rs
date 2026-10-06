//! UniFFI façade for the `fauna.stats.get` WS-RPC kind — the Backups stats
//! popover's repository storage stats. The WS-RPC twin of the deleted
//! `GET /api/v1/stats[?folder=…]` HTTP (`api-layers.md` § Stats).
//!
//! [`FfiStatsClient`] wraps `fauna_client_stats::StatsClient` (which wraps the
//! shared `NestClient`); [`FfiStatsReply`] mirrors the two-shape
//! `fauna_protocol::stats::StatsGetReply` enum (nest-wide `Global` totals vs a
//! single folder's `Folder` stats). The dedup ratio rides as
//! `dedup_ratio_micro: i64` (floats are forbidden on the dag-cbor wire); the
//! caller divides by 1e6 at the UI edge (matching the Rust-native Linux app
//! and the web SPA, which call the same kind).

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_stats::StatsClient;
use fauna_client_stats::stats::StatsGetReply;

use crate::{FfiError, general_err, stringify};

// ── reply mirrors ────────────────────────────────────────────────────────

/// FFI mirror of [`fauna_protocol::stats::BlobTypeCounts`] — the global
/// reply's blob-type breakdown.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiBlobTypeCounts {
    pub chunk: i64,
    pub manifest: i64,
}

/// FFI mirror of [`fauna_protocol::stats::StatsGetReply`] — one of two shapes,
/// selected by the request's `folder`. `dedup_ratio_micro` is the wire's
/// integer-scaled ratio (× 1e6); divide by 1e6 at the UI edge.
#[derive(uniffi::Enum, Clone, Debug, PartialEq)]
pub enum FfiStatsReply {
    /// `folder` omitted → nest-wide totals.
    Global {
        total_size_bytes: i64,
        total_blobs: i64,
        total_snapshots: i64,
        total_folders: i64,
        dedup_ratio_micro: i64,
        blob_types: FfiBlobTypeCounts,
    },
    /// `folder` present → that folder's stats.
    Folder {
        folder: String,
        snapshot_count: i64,
        /// `created_at` of the latest snapshot; `None` when the set has none.
        latest_snapshot: Option<i64>,
        total_files: i64,
        raw_size_bytes: i64,
        stored_size_bytes: i64,
        dedup_ratio_micro: i64,
        /// `"s3"` if any destination is S3, else `"local"`.
        storage_backend: String,
    },
}

/// A scope a newer nest added is an error — stats unavailable — never a case
/// the apps render (`transport.md` § Schema and forward-compat discipline,
/// rule 3); the mirror itself stays closed.
impl TryFrom<StatsGetReply> for FfiStatsReply {
    type Error = FfiError;
    fn try_from(r: StatsGetReply) -> Result<Self, FfiError> {
        Ok(match r {
            StatsGetReply::Global {
                total_size_bytes,
                total_blobs,
                total_snapshots,
                total_folders,
                dedup_ratio_micro,
                blob_types,
            } => FfiStatsReply::Global {
                total_size_bytes,
                total_blobs,
                total_snapshots,
                total_folders,
                dedup_ratio_micro,
                blob_types: FfiBlobTypeCounts {
                    chunk: blob_types.chunk,
                    manifest: blob_types.manifest,
                },
            },
            StatsGetReply::Folder {
                folder,
                snapshot_count,
                latest_snapshot,
                total_files,
                raw_size_bytes,
                stored_size_bytes,
                dedup_ratio_micro,
                storage_backend,
            } => FfiStatsReply::Folder {
                folder,
                snapshot_count,
                latest_snapshot,
                total_files,
                raw_size_bytes,
                stored_size_bytes,
                dedup_ratio_micro,
                storage_backend,
            },
            StatsGetReply::Unknown => {
                return Err(general_err(
                    "stats unavailable: the nest answered with a scope this app does not know",
                ));
            }
        })
    }
}

// ── FfiStatsClient ─────────────────────────────────────────────────────────

/// UniFFI handle for the `fauna.stats.get` kind. Construct via
/// [`crate::nest_client::FfiNestClient::stats`]; the method is exposed to
/// Swift as `async throws` and Kotlin as `suspend fun`.
#[derive(uniffi::Object)]
pub struct FfiStatsClient {
    nest: Arc<NestClient>,
}

impl FfiStatsClient {
    pub(crate) fn from_nest(nest: Arc<NestClient>) -> Arc<Self> {
        Arc::new(Self { nest })
    }

    fn client(&self) -> StatsClient<Arc<NestClient>> {
        StatsClient::new(Arc::clone(&self.nest))
    }
}

#[fauna_uniffi_async::export]
impl FfiStatsClient {
    /// `fauna.stats.get` — repository storage stats. `folder: Some(name)`
    /// returns that set's [`FfiStatsReply::Folder`] stats; `None` returns the
    /// nest-wide [`FfiStatsReply::Global`] totals.
    pub async fn stats_get(&self, folder: Option<String>) -> Result<FfiStatsReply, FfiError> {
        let reply = self.client().get(folder).await.map_err(stringify)?;
        reply.try_into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_stats::stats::BlobTypeCounts;

    #[test]
    fn folder_reply_maps() {
        let wire = StatsGetReply::Folder {
            folder: "documents".into(),
            snapshot_count: 3,
            latest_snapshot: Some(1_700_000_000),
            total_files: 42,
            raw_size_bytes: 4096,
            stored_size_bytes: 2048,
            dedup_ratio_micro: 2_000_000,
            storage_backend: "local".into(),
        };
        match FfiStatsReply::try_from(wire).unwrap() {
            FfiStatsReply::Folder {
                snapshot_count,
                dedup_ratio_micro,
                storage_backend,
                ..
            } => {
                assert_eq!(snapshot_count, 3);
                assert_eq!(dedup_ratio_micro, 2_000_000);
                assert_eq!(storage_backend, "local");
            }
            other => panic!("expected Folder, got {other:?}"),
        }
    }

    #[test]
    fn global_reply_maps_blob_types() {
        let wire = StatsGetReply::Global {
            total_size_bytes: 1024,
            total_blobs: 10,
            total_snapshots: 2,
            total_folders: 1,
            dedup_ratio_micro: 1_500_000,
            blob_types: BlobTypeCounts {
                chunk: 8,
                manifest: 2,
                extra: Default::default(),
            },
        };
        match FfiStatsReply::try_from(wire).unwrap() {
            FfiStatsReply::Global { blob_types, .. } => {
                assert_eq!(blob_types.chunk, 8);
                assert_eq!(blob_types.manifest, 2);
            }
            other => panic!("expected Global, got {other:?}"),
        }
    }
}
