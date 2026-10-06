//! Typed-call wrapper for the `fauna.stats.get` WS-RPC kind — repository
//! storage stats (the Backups stats popover + any admin storage view).
//!
//! The WS-RPC twin of the deleted `GET /api/v1/stats[?folder=…]` HTTP
//! route (residue sweep, `api-layers.md` § Stats). One kind, two reply
//! shapes selected by the request's `folder`: nest-wide [`StatsGetReply::Global`]
//! totals (`folder == None`) vs the named set's [`StatsGetReply::Folder`]
//! stats (`folder == Some`). The twin's `dedup_ratio: f64` rides as
//! `dedup_ratio_micro: i64` (floats are forbidden on the dag-cbor wire);
//! divide by [`fauna_protocol::stats::DEDUP_RATIO_MICRO_SCALE`] client-side.
//!
//! First lifted for the Apple Backups page (`apps/fauna-apple` — apple was
//! runtime-broken on the deleted route); the other apps lift this crate
//! rather than reimplement
//! the kind-composition (priority #1/#2).
//!
//! Pattern: same shape as `fauna-client-snapshots` — a thin
//! `StatsClient<R: RpcRequester>`, one async method per kind, no state
//! machine, wasm-clean (no `fauna-client` dependency).

use fauna_protocol::RpcRequester;
use fauna_protocol::stats::{StatsGetReply, StatsGetRequest};

pub use fauna_protocol::stats;

/// Typed `fauna.stats.*` call surface, generic over the WS-RPC transport
/// (`R: RpcRequester`): native call sites pass `Arc<NestClient>`, the wasm
/// SPA passes its `WsRpcClient`. The kind-composition logic is written once
/// here and shared across native + wasm (priority #2). Errors propagate as
/// the transport's `R::Error`.
pub struct StatsClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> StatsClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.stats.get` — repository storage stats. `folder == None` →
    /// nest-wide totals ([`StatsGetReply::Global`]); `Some(name)` → that
    /// folder's stats ([`StatsGetReply::Folder`]). Replay-safe pure read.
    pub async fn get(&self, folder: Option<String>) -> Result<StatsGetReply, R::Error> {
        self.nest
            .request(
                "fauna.stats.get",
                fauna_protocol::folders::addressed(StatsGetRequest {
                    folder,
                    ..Default::default()
                }),
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{MockRequester, RecordingRequester, block_on};

    #[test]
    fn constructor_builds_over_generic_requester() {
        let _c = StatsClient::new(MockRequester);
    }

    // ── Wire-contract test ──────────────────────────────────────────────────

    /// This crate's reply table for the shared [`RecordingRequester`]: a canned
    /// `StatsGetReply::Folder` so `get` resolves.
    fn reply(kind: &'static str) -> Vec<u8> {
        match kind {
            "fauna.stats.get" => fauna_protocol::encode_canonical(&StatsGetReply::Folder {
                folder: "documents".into(),
                snapshot_count: 3,
                latest_snapshot: Some(1_700_000_000),
                total_files: 42,
                raw_size_bytes: 4096,
                stored_size_bytes: 2048,
                dedup_ratio_micro: 2_000_000,
                storage_backend: "local".into(),
            }),
            other => panic!("RecordingRequester: unhandled kind {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    #[test]
    fn get_issues_stats_get_with_folder() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = StatsClient::new(std::sync::Arc::clone(&rec));
        let reply = block_on(client.get(Some("documents".into()))).expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.stats.get");
        let decoded: StatsGetRequest = fauna_protocol::decode_strict(&payload).unwrap();
        assert!(fauna_protocol::folders::SetAddressed::addresses(
            &decoded,
            "documents"
        ));
        assert!(matches!(reply, StatsGetReply::Folder { .. }));
    }
}
