//! Owner-side WS-RPC seam for the `fauna.labelers.{list,inspect,subscribe,
//! unsubscribe,publish}` community-labeler-registry methods — the thin
//! transport wrapper the labeler-catalog manager ([`crate::LabelerCatalogManager`])
//! calls to browse, inspect-before-subscribe, and (un)subscribe.
//!
//! **`publish` is a human client action for exactly one artifact kind.** It
//! reads as an algorithm-service-actor operation — and
//! content-moderation-and-ranking.md § Tier-3 still says "no net-new *human*
//! UI" of the **generic** publish, which is why the WASM path has no client
//! caller. But **D8** (that doc's § Resolved design decisions 2026-07-12,
//! user-ratified) carves out one: a user publishing their own trained topic
//! factor as a **List**, "an explicit voluntary act riding the existing
//! `fauna.labelers.publish` + inspect-before-subscribe trust gate". The
//! caller is the Personalization home's publish sheet, via
//! `fauna_client_personalization::publish`; mechanism owner is
//! `behavior/topic-factors.md` § Publishing a trained factor.
//!
//! **Pattern:** the wasm-clean generic `BridgesClient`/`CapabilitiesClient`
//! seam (`libs/fauna-client-capabilities/src/rpc.rs`) — `struct .. <R:
//! RpcRequester>`, one `async fn` per kind delegating to
//! `self.nest.request(kind, typed_req)`, no state machine, no concrete
//! transport (native `Arc<NestClient>` / wasm `WsRpcClient` injected by the
//! caller via the `Arc<T>` blanket impl).
//!
//! Design tracked internally; wire types: `fauna_protocol::labelers`.

use fauna_protocol::ByteBuf;
use fauna_protocol::RpcRequester;
use fauna_protocol::labelers::{
    InspectLabelerReply, InspectLabelerRequest, ListLabelersReply, ListLabelersRequest,
    PublishLabelerReply, PublishLabelerRequest, SubscribeLabelerReply, SubscribeLabelerRequest,
    UnsubscribeLabelerReply, UnsubscribeLabelerRequest,
};

/// A thin wrapper over the `fauna.labelers.{list,inspect,subscribe,
/// unsubscribe}` WS-RPC kinds, generic over the `R: RpcRequester` transport —
/// the same seam `CapabilitiesClient` uses, so it stays wasm-clean (no
/// `fauna-client` / `fauna-rpc-wasm` dependency): native call sites pass
/// `Arc<NestClient>`, the wasm SPA passes `WsRpcClient`, both satisfying
/// `R: RpcRequester`.
pub struct LabelersClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> LabelersClient<R> {
    /// Wrap a transport that can talk to the caller's nest.
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.labelers.list` — browse the catalog (metadata only, no WASM
    /// bytes).
    pub async fn list(&self) -> Result<ListLabelersReply, R::Error> {
        self.nest
            .request(
                "fauna.labelers.list",
                ListLabelersRequest {
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.labelers.inspect` — fetch one labeler's full signed record +
    /// WASM bytes, for the inspect-before-subscribe trust gate.
    pub async fn inspect(&self, labeler_id: Vec<u8>) -> Result<InspectLabelerReply, R::Error> {
        self.nest
            .request(
                "fauna.labelers.inspect",
                InspectLabelerRequest {
                    labeler_id: ByteBuf::from(labeler_id),
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.labelers.publish` — deposit a signed artifact on the caller's
    /// nest registry (class `User`). `metadata_blob` is the canonical-CBOR
    /// signed `AlgorithmLabeler` (build it with
    /// `fauna_ffi::build_signed_labeler_metadata` or
    /// `fauna_core::scoring::sign_labeler_metadata`), and `artifact_bytes` is
    /// what its `wasm_hash`/`wasm_size` bind — the WASM module for
    /// `artifact_kind::WASM`, or the `LabelerListArtifact` bytes (built by
    /// `fauna_core::scoring::build_list_artifact`) for `artifact_kind::LIST`.
    ///
    /// The wire field is named `wasm_bytes` for compatibility and reads
    /// "artifact bytes"; a List never touches the sandbox
    /// (content-moderation-and-ranking.md § Tier-3 artifact kinds). `list` is
    /// public-post-only — the nest rejects a `mail` List as malformed.
    pub async fn publish(
        &self,
        metadata_blob: Vec<u8>,
        artifact_bytes: Vec<u8>,
        content_kind: &str,
        artifact_kind: &str,
    ) -> Result<PublishLabelerReply, R::Error> {
        self.nest
            .request(
                "fauna.labelers.publish",
                PublishLabelerRequest {
                    metadata_blob: ByteBuf::from(metadata_blob),
                    wasm_bytes: ByteBuf::from(artifact_bytes),
                    content_kind: content_kind.to_string(),
                    artifact_kind: artifact_kind.to_string(),
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.labelers.subscribe` (owner == caller) — record a subscription
    /// and register the labeler's version so the re-score obligation scan
    /// owes a re-score. `grant_id` links the one capability grant the client
    /// minted for a restricted kind; `None` for a public-only subscription
    /// (no grant needed).
    pub async fn subscribe(
        &self,
        labeler_id: Vec<u8>,
        grant_id: Option<[u8; 16]>,
    ) -> Result<SubscribeLabelerReply, R::Error> {
        self.nest
            .request(
                "fauna.labelers.subscribe",
                SubscribeLabelerRequest {
                    labeler_id: ByteBuf::from(labeler_id),
                    grant_id: grant_id.map(|g| ByteBuf::from(g.to_vec())),
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.labelers.unsubscribe` (owner == caller) — drop the
    /// subscription (idempotent). The caller separately revokes any minted
    /// capability grant via `fauna.capabilities.revoke` (out of scope here —
    /// `fauna-client-capabilities::CapabilitiesClient::revoke`), taking the
    /// drain dark.
    pub async fn unsubscribe(
        &self,
        labeler_id: Vec<u8>,
    ) -> Result<UnsubscribeLabelerReply, R::Error> {
        self.nest
            .request(
                "fauna.labelers.unsubscribe",
                UnsubscribeLabelerRequest {
                    labeler_id: ByteBuf::from(labeler_id),
                    extra: Default::default(),
                },
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{RecordingRequester, block_on};
    use fauna_protocol::labelers::LabelerSummary;

    /// This crate's reply table for the shared [`RecordingRequester`]:
    /// one arm per kind, each the minimal valid shape its `Reply` decodes.
    fn reply(kind: &'static str) -> Vec<u8> {
        match kind {
            "fauna.labelers.list" => fauna_protocol::encode_canonical(&ListLabelersReply {
                labelers: vec![LabelerSummary {
                    labeler_id: ByteBuf::from(vec![0xAAu8; 32]),
                    version: 3,
                    publisher_actor: ByteBuf::from(vec![0xBBu8; 32]),
                    content_kind: "post".into(),
                    factor: "labeler:aabb".into(),
                    wasm_hash: ByteBuf::from(vec![0xCCu8; 36]),
                    wasm_size: 4096,
                    subscribed: false,
                    ..Default::default()
                }],
                extra: Default::default(),
            }),
            "fauna.labelers.inspect" => fauna_protocol::encode_canonical(&InspectLabelerReply {
                metadata_blob: ByteBuf::from(vec![0x11u8; 8]),
                wasm_bytes: ByteBuf::from(vec![0x22u8; 16]),
                ..Default::default()
            }),
            "fauna.labelers.subscribe" => {
                fauna_protocol::encode_canonical(&SubscribeLabelerReply {
                    factor: "labeler:aabb".into(),
                    ok: true,
                    extra: Default::default(),
                })
            }
            "fauna.labelers.unsubscribe" => {
                fauna_protocol::encode_canonical(&UnsubscribeLabelerReply {
                    ok: true,
                    extra: Default::default(),
                })
            }
            "fauna.labelers.publish" => fauna_protocol::encode_canonical(&PublishLabelerReply {
                labeler_id: ByteBuf::from(vec![0xAAu8; 32]),
                version: 1,
                ok: true,
                extra: Default::default(),
            }),
            other => panic!("unexpected kind {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    #[test]
    fn list_composes_kind_with_no_body() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = LabelersClient::new(rec.clone());

        let reply = block_on(client.list()).expect("infallible mock");
        assert_eq!(reply.labelers.len(), 1);
        assert_eq!(reply.labelers[0].content_kind, "post");
        assert_eq!(reply.labelers[0].factor, "labeler:aabb");

        let (kind, _) = rec.recorded();
        assert_eq!(kind, "fauna.labelers.list");
    }

    #[test]
    fn inspect_composes_kind_and_labeler_id() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = LabelersClient::new(rec.clone());
        let labeler_id = vec![0xAAu8; 32];

        let reply = block_on(client.inspect(labeler_id.clone())).expect("infallible mock");
        assert_eq!(reply.wasm_bytes.as_ref(), &[0x22u8; 16]);

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.labelers.inspect");
        let req: InspectLabelerRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.labeler_id.as_ref(), labeler_id.as_slice());
    }

    #[test]
    fn subscribe_with_no_grant_composes_none() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = LabelersClient::new(rec.clone());
        let labeler_id = vec![0xAAu8; 32];

        let reply = block_on(client.subscribe(labeler_id.clone(), None)).expect("infallible mock");
        assert!(reply.ok);
        assert_eq!(reply.factor, "labeler:aabb");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.labelers.subscribe");
        let req: SubscribeLabelerRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.labeler_id.as_ref(), labeler_id.as_slice());
        assert!(
            req.grant_id.is_none(),
            "public-only subscribe carries no grant"
        );
    }

    #[test]
    fn subscribe_with_grant_composes_grant_id() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = LabelersClient::new(rec.clone());
        let labeler_id = vec![0xAAu8; 32];
        let grant_id = [0x77u8; 16];

        block_on(client.subscribe(labeler_id, Some(grant_id))).expect("infallible mock");

        let (_, payload) = rec.recorded();
        let req: SubscribeLabelerRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.grant_id.expect("grant present").as_ref(), &grant_id);
    }

    #[test]
    fn publish_composes_kind_artifact_bytes_and_both_kind_axes() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = LabelersClient::new(rec.clone());
        let metadata = vec![0x11u8; 8];
        let artifact = vec![0x22u8; 24];

        // The wire strings literally, not `fauna_core::scoring::artifact_kind`:
        // this seam is transport-level and stays dependency-light, and the
        // assertion below is precisely that what the caller passes reaches the
        // wire verbatim.
        let reply = block_on(client.publish(metadata.clone(), artifact.clone(), "post", "list"))
            .expect("infallible mock");
        assert!(reply.ok);
        assert_eq!(reply.labeler_id.as_ref(), &[0xAAu8; 32]);

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.labelers.publish");
        let req: PublishLabelerRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.metadata_blob.as_ref(), metadata.as_slice());
        assert_eq!(
            req.wasm_bytes.as_ref(),
            artifact.as_slice(),
            "the artifact bytes ride the compat-named wasm_bytes field verbatim"
        );
        assert_eq!(req.content_kind, "post");
        assert_eq!(
            req.artifact_kind, "list",
            "a List must not default to the wasm kind — that would send it to the sandbox gate"
        );
    }

    #[test]
    fn unsubscribe_composes_kind_and_labeler_id() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = LabelersClient::new(rec.clone());
        let labeler_id = vec![0xAAu8; 32];

        let reply = block_on(client.unsubscribe(labeler_id.clone())).expect("infallible mock");
        assert!(reply.ok);

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.labelers.unsubscribe");
        let req: UnsubscribeLabelerRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.labeler_id.as_ref(), labeler_id.as_slice());
    }
}
