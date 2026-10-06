//! Typed-call wrapper for the one protocol-unique consume-side Bluesky
//! WS-RPC kind — `bluesky.feed.thread`. Fetches a crossposted post's full
//! reply context (ancestors oldest-first, the focal post, then its direct
//! replies) as a **flat** list of translated posts — `translate_thread`, not
//! a nested tree.
//!
//! Faithful transport migration of the two deleted HTTP twins
//! (`GET /api/v1/bluesky/feed/thread/{uri}` + `GET /api/v1/bluesky/thread?post_id={hex}`)
//! per `docs/goal/behavior/bridges.md` § Bluesky-native thread view and
//! `docs/goal/architecture/api-layers.md` § Bluesky. The android + linux
//! post-detail surfaces are the consumers (the `bluesky.*` auth/settings
//! twins fold into the unified `fauna.bridges.*`, so they get no client crate
//! here).
//!
//! Pattern: same shape as the sibling per-feature client wrappers
//! (`fauna-client-inbox`, `-search`, `-bridges`, `-conversations`) — a thin
//! `pub struct BlueskyClient { nest: R }`, one async method per kind, no state
//! machine. Generic over the WS-RPC transport (`R: RpcRequester`): native
//! call sites pass `Arc<NestClient>` (linux directly; android via the
//! `fauna-ffi` `FfiBlueskyClient` seam), a wasm SPA would pass its
//! `WsRpcClient`. The kind-composition logic is written once here and shared
//! (priority #2).
//!
//! Consume side only: the hosted identity (did:plc custody, rotation keys,
//! handle binding, tombstones) is protocol-level and lives in
//! `fauna-client-atproto` (`docs/goal/behavior/atproto-pds-bridge.md` § Naming).

use fauna_protocol::RpcRequester;
use fauna_protocol::bluesky::{BlueskyThreadReply, BlueskyThreadRequest};

pub use fauna_protocol::bluesky;

/// Typed `bluesky.feed.thread` call surface. Caller-scoped by construction
/// (no `actor_id` param — the connection knows its caller; the handler
/// restores *that* actor's Bluesky OAuth agent). Errors propagate as the
/// transport's `R::Error` (native `NestClientError`, wasm rpc-wasm error);
/// the `bluesky.{not_found,blocked,upstream}` `RpcError`s the handler emits
/// surface through that error channel.
pub struct BlueskyClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> BlueskyClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `bluesky.feed.thread` — fetch the thread context for the post addressed
    /// by `req`. `AtUri { uri }` is resolved as-is; `PostId { post_id }` (raw
    /// 32-byte Fauna post id) is hex-encoded server-side and resolved to its
    /// AT-URI through the `bluesky_posts` crosspost mapping (a missing mapping
    /// is `bluesky.feed.not_found`). Replay-safe read, 30 s handler deadline
    /// (it makes a live `getPostThread` XRPC round-trip). The reply's `posts`
    /// is the flat list: ancestors oldest-first, the focal post, then its
    /// direct replies.
    pub async fn thread(&self, req: BlueskyThreadRequest) -> Result<BlueskyThreadReply, R::Error> {
        self.nest.request("bluesky.feed.thread", req).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{MockRequester, RecordingRequester, block_on};

    #[test]
    fn constructor_builds_over_generic_requester() {
        let _c = BlueskyClient::new(MockRequester);
    }

    // ── Wire-contract tests ─────────────────────────────────────────────────
    //
    // Pin the exact kind string + that both request variants round-trip to the
    // typed `BlueskyThreadRequest`, so a kind rename here can't silently break
    // the adapter. Real end-to-end round-trip conformance lives in
    // `bins/fauna-nest/tests/conformance_bluesky.rs` (real router dispatch).

    use fauna_protocol::bluesky::BlueskyPost;

    /// This crate's reply table for the shared [`RecordingRequester`]:
    /// one arm per kind, each the minimal valid shape its `Reply` decodes.
    fn reply(kind: &'static str) -> Vec<u8> {
        match kind {
            "bluesky.feed.thread" => fauna_protocol::encode_canonical(&BlueskyThreadReply {
                extra: Default::default(),
                focal_index: 0,
                posts: vec![BlueskyPost::default()],
            }),
            other => panic!("RecordingRequester: unhandled kind {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    #[test]
    fn thread_by_post_id_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = BlueskyClient::new(rec.clone());
        block_on(client.thread(BlueskyThreadRequest::PostId {
            post_id: vec![0xab; 32],
        }))
        .expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "bluesky.feed.thread");
        let req: BlueskyThreadRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(
            req,
            BlueskyThreadRequest::PostId {
                post_id: vec![0xab; 32],
            }
        );
    }

    #[test]
    fn thread_by_at_uri_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = BlueskyClient::new(rec.clone());
        block_on(client.thread(BlueskyThreadRequest::AtUri {
            uri: "at://did:plc:abc/app.bsky.feed.post/xyz".into(),
        }))
        .expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "bluesky.feed.thread");
        let req: BlueskyThreadRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(
            req,
            BlueskyThreadRequest::AtUri {
                uri: "at://did:plc:abc/app.bsky.feed.post/xyz".into(),
            }
        );
    }
}
