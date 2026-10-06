//! Typed-call wrapper for the user-facing `fauna.notifications.*` WS-RPC
//! kinds — the list / mark-read / count plane clients hit from the
//! notifications inbox + unread badge. Part of the WS-RPC-everywhere
//! migration (tracked internally).
//!
//! Pattern: same shape as `fauna-client-feed` / `fauna-client-posts` — a thin
//! `pub struct NotificationsClient<R: RpcRequester> { nest: R }`, one async
//! method per kind, no state machine, generic over the WS-RPC transport so the
//! kind-composition logic is written once and shared across native + wasm
//! (priority #2). This crate is the transport surface only; the notifications
//! inbox view-model lives in the client's shared state layer.

use fauna_protocol::RpcRequester;
use fauna_protocol::notifications::{
    NotifClearReply, NotifClearRequest, NotifCountReply, NotifCountRequest, NotifDismissReply,
    NotifDismissRequest, NotifListReply, NotifListRequest, NotifMarkReadReply,
    NotifMarkReadRequest,
};

pub use fauna_protocol::notifications;

mod destination;
mod text;
pub use destination::{NotificationDestination, notification_destination};
pub use text::{NotificationText, knock_push_text, notification_push_text, notification_text};

/// Typed `fauna.notifications.*` call surface, generic over the WS-RPC
/// transport (`R: RpcRequester`): native call sites pass `Arc<NestClient>`, the
/// wasm SPA passes its `WsRpcClient`. Errors propagate as the transport's
/// `R::Error` (native `NestClientError`, wasm rpc-wasm error).
pub struct NotificationsClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> NotificationsClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.notifications.list` — fetch a page of the calling actor's
    /// unified notification history (all protocols), newest first. `cursor`
    /// is the last-seen notification `id` (None ⇒ newest page); `limit` is
    /// the page size (None ⇒ the nest applies its default 25 + 1..=100
    /// clamp). The reply carries the rows + the next-page `cursor`. Pure
    /// read; replay-safe at 5 s.
    pub async fn notifications_list(
        &self,
        cursor: Option<i64>,
        limit: Option<i64>,
    ) -> Result<NotifListReply, R::Error> {
        self.nest
            .request(
                "fauna.notifications.list",
                NotifListRequest {
                    cursor,
                    limit,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.notifications.mark_read` — mark all of the calling actor's
    /// notifications created at or before `up_to` (micros since epoch) as
    /// read. `up_to = None` ⇒ mark everything up to now. The reply echoes
    /// `marked_read` (the number of rows flipped). Idempotent upsert;
    /// replay-safe at 5 s.
    pub async fn notifications_mark_read(
        &self,
        up_to: Option<i64>,
    ) -> Result<NotifMarkReadReply, R::Error> {
        self.nest
            .request(
                "fauna.notifications.mark_read",
                NotifMarkReadRequest {
                    up_to,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.notifications.count` — the calling actor's unread notification
    /// count (for the unread badge). Pure read; replay-safe at 5 s.
    pub async fn notifications_count(&self) -> Result<NotifCountReply, R::Error> {
        self.nest
            .request(
                "fauna.notifications.count",
                NotifCountRequest {
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.notifications.dismiss` — delete one of the calling actor's
    /// rows by `id` (`behavior/notifications.md` § Retention, rule 2: nothing
    /// on the nest sweeps a row; the user's own dismiss is the delete). The
    /// reply's `dismissed` is `false` when no row of that id was the caller's
    /// — idempotent, never an error for that case. The one refusal: a
    /// `security.notice` inside its window answers
    /// `fauna_protocol::notifications::CODE_NOTIFICATION_RETAINED`
    /// (§ Retention, the security-notice window). Replay-safe at 5 s.
    pub async fn notifications_dismiss(&self, id: i64) -> Result<NotifDismissReply, R::Error> {
        self.nest
            .request(
                "fauna.notifications.dismiss",
                NotifDismissRequest {
                    id,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.notifications.clear` — delete every row of the calling actor's
    /// created at or before `up_to` (micros since epoch; `None` ⇒ everything
    /// up to now). Pass the newest `created_at` the page has shown so a row
    /// that arrived after it looked survives. The reply echoes `cleared`; a
    /// `security.notice` inside its window is skipped, not counted.
    /// Replay-safe at 5 s.
    pub async fn notifications_clear(
        &self,
        up_to: Option<i64>,
    ) -> Result<NotifClearReply, R::Error> {
        self.nest
            .request(
                "fauna.notifications.clear",
                NotifClearRequest {
                    up_to,
                    extra: std::collections::BTreeMap::new(),
                },
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
        let _c = NotificationsClient::new(MockRequester);
    }

    // ── Wire-contract tests ─────────────────────────────────────────────────
    //
    // The construction-only `MockRequester` above can't catch a wrong kind
    // string or a request that no longer serializes to the shape the nest
    // handler decodes. These tests pin both: each `NotificationsClient` method
    // must send its exact `fauna.notifications.*` kind and a payload that
    // round-trips back to the typed request. No nest-side conformance test
    // routes through this adapter's literal kind strings, so an adapter-method
    // kind rename would otherwise break the notifications inbox + unread badge
    // silently. The pattern mirrors `fauna-client-events` / `-snapshots` /
    // `-sync`'s `RecordingRequester` (transport-free, so it runs on every
    // target including wasm); real end-to-end round-trip conformance lives in
    // `bins/fauna-nest/tests/conformance_notifications.rs`.

    /// This crate's reply table for the shared [`RecordingRequester`]:
    /// one arm per kind, each the minimal valid shape its `Reply` decodes.
    fn reply(kind: &'static str) -> Vec<u8> {
        // Answer with a reply the requested `Reply` type decodes — one arm
        // per kind, each the minimal valid shape.
        match kind {
            "fauna.notifications.list" => fauna_protocol::encode_canonical(&NotifListReply {
                notifications: vec![],
                cursor: None,
                extra: Default::default(),
            }),
            "fauna.notifications.mark_read" => {
                fauna_protocol::encode_canonical(&NotifMarkReadReply {
                    marked_read: 0,
                    extra: Default::default(),
                })
            }
            "fauna.notifications.count" => fauna_protocol::encode_canonical(&NotifCountReply {
                count: 0,
                extra: Default::default(),
            }),
            "fauna.notifications.dismiss" => fauna_protocol::encode_canonical(&NotifDismissReply {
                dismissed: true,
                extra: Default::default(),
            }),
            "fauna.notifications.clear" => fauna_protocol::encode_canonical(&NotifClearReply {
                cleared: 0,
                extra: Default::default(),
            }),
            other => panic!("RecordingRequester: unhandled kind {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    #[test]
    fn list_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = NotificationsClient::new(rec.clone());
        block_on(client.notifications_list(Some(42), Some(25))).expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.notifications.list");
        let req: NotifListRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.cursor, Some(42));
        assert_eq!(req.limit, Some(25));
    }

    #[test]
    fn mark_read_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = NotificationsClient::new(rec.clone());
        block_on(client.notifications_mark_read(Some(1_700_000_000_000_000)))
            .expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.notifications.mark_read");
        let req: NotifMarkReadRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.up_to, Some(1_700_000_000_000_000));
    }

    #[test]
    fn count_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = NotificationsClient::new(rec.clone());
        block_on(client.notifications_count()).expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.notifications.count");
        let _req: NotifCountRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
    }

    #[test]
    fn dismiss_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = NotificationsClient::new(rec.clone());
        let reply = block_on(client.notifications_dismiss(42)).expect("infallible mock");
        assert!(reply.dismissed);

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.notifications.dismiss");
        let req: NotifDismissRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.id, 42);
    }

    #[test]
    fn clear_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = NotificationsClient::new(rec.clone());
        block_on(client.notifications_clear(Some(1_700_000_000_000_000))).expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.notifications.clear");
        let req: NotifClearRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.up_to, Some(1_700_000_000_000_000));
    }
}
