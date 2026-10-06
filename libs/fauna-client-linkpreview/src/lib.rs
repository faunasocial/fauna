//! Typed-call wrapper for the user-facing `fauna.linkpreview.resolve` WS-RPC
//! kind — the link-preview resolver the render managers call to turn a
//! `RenderBlock::LinkPreview { Resolving }` into `Resolved` / `Failed`
//! (`docs/goal/architecture/render-model.md` § D4).
//!
//! Pattern: same shape as `fauna-client-posts` — a thin
//! `pub struct LinkPreviewClient<R: RpcRequester> { nest: R }`, one async method
//! for the one kind, no state machine, generic over the WS-RPC transport so the
//! kind-composition logic is written once and shared across native + wasm
//! (priority #2). This crate is the transport surface only; the
//! fold-into-document + notify discipline lives in each render manager
//! (`fauna_feed::FeedManager`, later `fauna_conversations::ConversationsManager`).

use fauna_protocol::RpcRequester;
use fauna_protocol::linkpreview::{
    KIND_LINKPREVIEW_RESOLVE, LinkPreviewResolveReply, LinkPreviewResolveRequest,
};

pub use fauna_protocol::linkpreview;

/// Typed `fauna.linkpreview.resolve` call surface, generic over the WS-RPC
/// transport (`R: RpcRequester`): native call sites pass `Arc<NestClient>`, the
/// wasm SPA passes its `WsRpcClient`. Errors propagate as the transport's
/// `R::Error` (native `NestClientError`, wasm rpc-wasm error).
pub struct LinkPreviewClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> LinkPreviewClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.linkpreview.resolve` — resolve a bare URL's OpenGraph/meta preview.
    /// The nest fetches the page behind its SSRF/size/time guards, parses the
    /// OpenGraph/`<meta>` tags, stores any og:image as a content-addressed blob,
    /// and caches the result by url; the reply is
    /// [`LinkPreviewResolveReply::Resolved`] (title / description / og-image hash)
    /// or [`Failed`](LinkPreviewResolveReply::Failed) (fetch/parse failure, an
    /// SSRF/size/time block, or a page with no usable metadata). Authed (User
    /// class); replay-safe at 30 s. The caller maps the reply onto
    /// `fauna_core::render::PreviewState` and folds it into the matching
    /// `RenderBlock::LinkPreview` block.
    pub async fn resolve(
        &self,
        url: impl Into<String>,
    ) -> Result<LinkPreviewResolveReply, R::Error> {
        self.nest
            .request(
                KIND_LINKPREVIEW_RESOLVE,
                LinkPreviewResolveRequest {
                    url: url.into(),
                    extra: Default::default(),
                },
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{FailingRequester, block_on};
    use serde::Serialize;
    use serde::de::DeserializeOwned;
    use std::sync::{Arc, Mutex};

    /// Records the (kind, decoded url) of the one request and answers a canned
    /// reply of the caller's choosing — mirrors `fauna-client-posts`'
    /// `RecordingRequester` (held in an `Arc`, the only `RpcRequester` blanket).
    struct Mock {
        seen: Mutex<Option<(String, String)>>,
        reply: LinkPreviewResolveReply,
    }

    impl Mock {
        fn answering(reply: LinkPreviewResolveReply) -> Self {
            Self {
                seen: Mutex::new(None),
                reply,
            }
        }
    }

    impl Default for Mock {
        fn default() -> Self {
            Self::answering(LinkPreviewResolveReply::Failed)
        }
    }

    impl RpcRequester for Mock {
        type Error = std::convert::Infallible;
        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: Serialize,
            Reply: DeserializeOwned,
        {
            // Round-trip the payload through the canonical codec to read its `url`.
            let bytes = fauna_protocol::encode_canonical(&payload).unwrap();
            let req: LinkPreviewResolveRequest = fauna_protocol::decode_strict(&bytes).unwrap();
            *self.seen.lock().unwrap() = Some((kind.to_string(), req.url));
            let reply_bytes = fauna_protocol::encode_canonical(&self.reply).unwrap();
            Ok(fauna_protocol::decode_strict(&reply_bytes).unwrap())
        }
    }

    #[test]
    fn constructor_builds_over_generic_requester() {
        let _c = LinkPreviewClient::new(Arc::new(Mock::default()));
    }

    #[test]
    fn resolve_composes_kind_and_payload() {
        let mock = Arc::new(Mock::default());
        let client = LinkPreviewClient::new(mock.clone());
        let reply = block_on(client.resolve("https://example.com/a")).unwrap();
        assert_eq!(reply, LinkPreviewResolveReply::Failed);
        let seen = mock
            .seen
            .lock()
            .unwrap()
            .clone()
            .expect("a call was recorded");
        assert_eq!(seen.0, "fauna.linkpreview.resolve");
        assert_eq!(seen.1, "https://example.com/a");
    }

    #[test]
    fn resolve_maps_resolved_reply_with_image() {
        let mock = Arc::new(Mock::answering(LinkPreviewResolveReply::Resolved {
            title: "An Article".into(),
            description: "Some description.".into(),
            image_hash: Some("aa01bb02".into()),
        }));
        let client = LinkPreviewClient::new(mock);
        let reply = block_on(client.resolve("https://example.com/b")).unwrap();
        assert_eq!(
            reply,
            LinkPreviewResolveReply::Resolved {
                title: "An Article".into(),
                description: "Some description.".into(),
                image_hash: Some("aa01bb02".into()),
            }
        );
    }

    #[test]
    fn resolve_maps_resolved_reply_without_image() {
        let mock = Arc::new(Mock::answering(LinkPreviewResolveReply::Resolved {
            title: "No Image Page".into(),
            description: String::new(),
            image_hash: None,
        }));
        let client = LinkPreviewClient::new(mock);
        let reply = block_on(client.resolve("https://example.com/c")).unwrap();
        assert_eq!(
            reply,
            LinkPreviewResolveReply::Resolved {
                title: "No Image Page".into(),
                description: String::new(),
                image_hash: None,
            }
        );
    }

    #[test]
    fn resolve_propagates_transport_error_unchanged() {
        let client = LinkPreviewClient::new(FailingRequester::new("transport unreachable"));
        let err = block_on(client.resolve("https://example.com/d")).unwrap_err();
        assert_eq!(err.0, "transport unreachable");
    }
}
