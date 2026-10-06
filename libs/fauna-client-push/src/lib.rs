//! Typed-call wrapper for the push-subscription-management WS-RPC kinds —
//! `fauna.push.{vapid_key,subscribe,unsubscribe}`. The per-device push
//! subscription surface the client drives: fetch the server's VAPID key for
//! `pushManager.subscribe`, then register / remove a device's subscription.
//!
//! Faithful transport migration of the three push-management HTTP routes
//! (`GET /api/v1/push/vapid-key`, `POST /api/v1/push/subscribe`,
//! `DELETE /api/v1/push/subscribe/{device_id}`) — the HTTP twins are
//! **deleted**; `fauna.push.*` is the sole surface
//! (`docs/goal/architecture/api-layers.md` § Push Notifications).
//!
//! Pattern: same shape as the sibling per-feature client wrappers
//! (`fauna-client-inbox`, `-moderation`, `-spam`) — a thin
//! `PushClient<R: RpcRequester>`, one async method per kind. The registration
//! state machine on top of it — the install intent bit, the which-actor record,
//! the leave-shape drops — is [`registration`] (lifted from web and apple).
//! Generic over the WS-RPC transport: native call sites pass
//! `Arc<NestClient>`, the wasm SPA passes its `WsRpcClient`. Written once,
//! shared across native + wasm (priority #2 — the native app push-
//! subscription paths reuse this rather than each re-implementing the kinds).
//!
//! The *actual push delivery* to Web Push / APNs endpoints is server-to-third-
//! party HTTP and is unaffected; these kinds cover only subscription
//! management. The browser-side `serviceWorker.register` / `PushManager.
//! subscribe` work stays in the client shell (it can't move to Rust); only the
//! three nest hops live here.

use fauna_protocol::RpcRequester;
use fauna_protocol::push::{
    SubscribeReply, SubscribeRequest, UnsubscribeReply, UnsubscribeRequest, VapidKeyReply,
    VapidKeyRequest,
};

pub use fauna_protocol::push;

pub mod registration;

/// Typed `fauna.push.*` call surface. `subscribe`/`unsubscribe` are actor-
/// scoped device management on the connection actor; `vapid_key` rides the
/// authenticated connection (push subscription is inherently post-login, so
/// there is no pre-identity caller). Errors propagate as the transport's
/// `R::Error`; the handler's `fauna.push.{unavailable,invalid_request}`
/// `RpcError`s surface through that error channel.
pub struct PushClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> PushClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.push.vapid_key` — fetch the server's base64url VAPID public key
    /// (the `applicationServerKey` for `pushManager.subscribe`).
    pub async fn vapid_key(&self) -> Result<VapidKeyReply, R::Error> {
        self.nest
            .request("fauna.push.vapid_key", VapidKeyRequest::default())
            .await
    }

    /// `fauna.push.subscribe` — register or update the connection actor's push
    /// subscription for one device (idempotent upsert). `transport` is
    /// `"web-push"` (default) or `"apns"`; for web-push, `key_p256dh`/`key_auth`
    /// are required (the handler validates).
    pub async fn subscribe(&self, req: SubscribeRequest) -> Result<SubscribeReply, R::Error> {
        self.nest.request("fauna.push.subscribe", req).await
    }

    /// `fauna.push.unsubscribe` — remove the connection actor's push
    /// subscription for `device_id`.
    pub async fn unsubscribe(
        &self,
        device_id: impl Into<String>,
    ) -> Result<UnsubscribeReply, R::Error> {
        self.nest
            .request(
                "fauna.push.unsubscribe",
                UnsubscribeRequest {
                    device_id: device_id.into(),
                    extra: Default::default(),
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
        let _c = PushClient::new(MockRequester);
    }

    // ── Wire-contract tests ─────────────────────────────────────────────────
    //
    // Pin the exact kind strings + that the payloads round-trip to the typed
    // requests, so a kind rename here can't silently break the adapter. Real
    // end-to-end dispatch lives in the nest's push handler tests (real router).

    /// This crate's reply table for the shared [`RecordingRequester`]:
    /// one arm per kind, each the minimal valid shape its `Reply` decodes.
    fn reply(kind: &'static str) -> Vec<u8> {
        match kind {
            "fauna.push.vapid_key" => fauna_protocol::encode_canonical(&VapidKeyReply {
                public_key: "BPaX-base64url".into(),
                extra: Default::default(),
            }),
            "fauna.push.subscribe" => fauna_protocol::encode_canonical(&SubscribeReply {
                ok: true,
                extra: Default::default(),
            }),
            "fauna.push.unsubscribe" => fauna_protocol::encode_canonical(&UnsubscribeReply {
                ok: true,
                extra: Default::default(),
            }),
            other => panic!("RecordingRequester: unhandled kind {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    #[test]
    fn vapid_key_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = PushClient::new(rec.clone());
        let reply = block_on(client.vapid_key()).expect("infallible mock");
        assert_eq!(reply.public_key, "BPaX-base64url");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.push.vapid_key");
        let _req: VapidKeyRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
    }

    #[test]
    fn subscribe_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = PushClient::new(rec.clone());
        let reply = block_on(client.subscribe(SubscribeRequest {
            device_id: "dev-1".into(),
            endpoint: "https://push.example.com/abc".into(),
            key_p256dh: Some("p256dh".into()),
            key_auth: Some("auth".into()),
            transport: None,
            extra: Default::default(),
        }))
        .expect("infallible mock");
        assert!(reply.ok);

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.push.subscribe");
        let req: SubscribeRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.device_id, "dev-1");
        assert_eq!(req.endpoint, "https://push.example.com/abc");
        assert_eq!(req.key_p256dh.as_deref(), Some("p256dh"));
    }

    #[test]
    fn unsubscribe_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = PushClient::new(rec.clone());
        block_on(client.unsubscribe("dev-1")).expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.push.unsubscribe");
        let req: UnsubscribeRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.device_id, "dev-1");
    }
}
