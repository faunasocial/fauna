//! WS-RPC production impl of the page seam, over
//! `fauna_client_labelers::LabelersClient` (the shared `fauna.labelers.*`
//! typed-call surface). This is the directive-correct
//! (`no-http-ws-rpc-everywhere`) consumer — no HTTP.
//!
//! Mirrors `fauna_devices_machine::nest_api::ws_rpc`: a generic
//! [`WsRpcLabelerCatalogNest<R>`] holds the kind-composition + wire→snapshot
//! transcription + error mapping once (priority #2); the per-target concrete
//! trait impls (native `Arc<NestClient>`, wasm `WsRpcClient`) and the
//! `build_labeler_catalog_machine` constructors live in the `cfg`-gated
//! submodules below and just delegate.

use std::sync::Arc;

use fauna_client_bridges::{HolderInfo, MailAdminClient, discover_holders};
use fauna_client_capabilities::rpc::CapabilitiesClient;
use fauna_client_labelers::LabelersClient;
use fauna_core::encoding::canonical_decode;
use fauna_core::format::hex_full;
use fauna_core::identity::ActorKeypair;
use fauna_core::scoring::{
    AlgorithmLabeler, artifact_kind, validate_list_artifact, validate_text_model_artifact,
    verify_labeler_metadata,
};
use fauna_protocol::{NestSeamError, RpcErrorClass, RpcRequester};

use super::{InspectResult, LabelerCatalogApiError, LabelerCatalogNestApi};
use crate::machine::{LabelerCatalogMachine, LabelerGrantSeams};
use crate::observer::LabelerCatalogObserver;
use crate::snapshots::{
    LabelerCatalogEntry, LabelerInspectListEntry, LabelerInspectModelNgram, LabelerInspectView,
};

/// Generic WS-RPC seam over any [`RpcRequester`]. Native binds
/// `R = Arc<NestClient>`, wasm `R = WsRpcClient`; the per-target trait impls
/// below delegate to these inherent methods so the logic is written once.
/// Holds the bare transport (both are cheap `Clone` handles) and wraps it in
/// the typed `fauna.labelers.*` / `fauna.capabilities.*` / holder-discovery
/// clients per call.
pub struct WsRpcLabelerCatalogNest<R: RpcRequester> {
    nest: R,
}

// `LabelerCatalogNestApi` requires `Debug`, but the client isn't `Debug`; the
// requester carries no renderable state, so a name-only impl satisfies the bound.
impl<R: RpcRequester> std::fmt::Debug for WsRpcLabelerCatalogNest<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WsRpcLabelerCatalogNest")
    }
}

impl<R> WsRpcLabelerCatalogNest<R>
where
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass + core::fmt::Display,
{
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    fn labelers(&self) -> LabelersClient<R> {
        LabelersClient::new(self.nest.clone())
    }

    async fn do_list(&self) -> Result<Vec<LabelerCatalogEntry>, LabelerCatalogApiError> {
        let reply = self.labelers().list().await.map_err(map_err)?;
        Ok(reply
            .labelers
            .into_iter()
            .map(|l| LabelerCatalogEntry {
                labeler_id: hex_full(l.labeler_id.as_ref()),
                version: l.version,
                publisher_actor: hex_full(l.publisher_actor.as_ref()),
                artifact_kind: l.artifact_kind,
                artifact_version: l.artifact_version,
                content_kind: l.content_kind,
                factor: l.factor,
                wasm_hash: hex_full(l.wasm_hash.as_ref()),
                wasm_size: l.wasm_size,
                subscribed: l.subscribed,
            })
            .collect())
    }

    async fn do_inspect(
        &self,
        labeler_id: Vec<u8>,
    ) -> Result<InspectResult, LabelerCatalogApiError> {
        let reply = self.labelers().inspect(labeler_id).await.map_err(map_err)?;
        let metadata: AlgorithmLabeler =
            canonical_decode(reply.metadata_blob.as_ref()).map_err(|e| {
                LabelerCatalogApiError::Transient {
                    detail: format!("decode labeler metadata: {e}"),
                }
            })?;
        // Re-verify client-side (hash/size/signature) rather than trusting the
        // nest's word for it — the inspect-before-subscribe transparency gate
        // holds even against a compromised/lying nest (the same
        // `verify_labeler_metadata` the nest's publish gate + the FFI holder
        // use, so the check can't silently drift between the three call sites).
        let mut verified = verify_labeler_metadata(&metadata, reply.wasm_bytes.as_ref()).is_ok();

        // For a `list` artifact, inspect's whole promise is the decoded content
        // (frame § Tier-3 artifact kinds: "the client renders the exact
        // id→score map before subscribing") — same nest-gate validator, so what
        // renders here is exactly what the nest materializes. Undecodable ⇒ not
        // inspectable ⇒ not verified.
        let artifact_kind = reply.artifact_kind.clone();
        let (list_name, list_entries) = if artifact_kind == artifact_kind::LIST {
            match validate_list_artifact(reply.wasm_bytes.as_ref()) {
                Ok(artifact) => (
                    artifact.name,
                    artifact
                        .entries
                        .iter()
                        .map(|e| LabelerInspectListEntry {
                            content_id: hex_full(e.content_id.as_ref() as &[u8]),
                            score: e.score,
                        })
                        .collect(),
                ),
                Err(_) => {
                    verified = false;
                    (None, Vec::new())
                }
            }
        } else {
            (None, Vec::new())
        };

        // The `text-model` twin. Inspect's promise for this kind is the FULL
        // vocabulary — it is the model's whole matching surface, and the one
        // thing a subscriber can weigh before letting it re-rank their feed.
        //
        // ⚠ An unknown artifact `version` is decoded and rendered like any
        // other, NOT folded into `verified: false`: the validator deliberately
        // accepts a future version (rejecting one would make an older nest
        // refuse a newer client's artifact), and the version is the
        // *subscriber's* tokenizer contract, resolved at the compose seam by
        // leaving the factor inert. Marking it unverified here would tell the
        // user their artifact is tampered with when it is merely newer.
        let (model_name, model_ngrams) = if artifact_kind == artifact_kind::TEXT_MODEL {
            match validate_text_model_artifact(reply.wasm_bytes.as_ref()) {
                Ok(artifact) => (
                    artifact.name,
                    artifact
                        .ngrams
                        .into_iter()
                        .map(|n| LabelerInspectModelNgram {
                            ngram: n.ngram,
                            more: n.more,
                            less: n.less,
                        })
                        .collect(),
                ),
                // Undecodable ⇒ not inspectable ⇒ not verified, the List's rule.
                Err(_) => {
                    verified = false;
                    (None, Vec::new())
                }
            }
        } else {
            (None, Vec::new())
        };

        Ok(InspectResult {
            view: LabelerInspectView {
                labeler_id: hex_full(&metadata.algorithm_id.0),
                version: metadata.version,
                artifact_kind,
                wasm_hash: hex_full(metadata.wasm_hash.as_bytes()),
                wasm_size: metadata.wasm_size,
                needs_text: metadata.input_schema.needs_text,
                needs_hashtags: metadata.input_schema.needs_hashtags,
                needs_media_metadata: metadata.input_schema.needs_media_metadata,
                needs_author: metadata.input_schema.needs_author,
                needs_attachment_bytes: metadata.input_schema.needs_attachment_bytes,
                verified,
                list_name,
                list_entries,
                model_name,
                model_ngrams,
            },
        })
    }

    async fn do_subscribe(
        &self,
        labeler_id: Vec<u8>,
        grant_id: Option<[u8; 16]>,
    ) -> Result<(), LabelerCatalogApiError> {
        self.labelers()
            .subscribe(labeler_id, grant_id)
            .await
            .map(|_| ())
            .map_err(map_err)
    }

    async fn do_unsubscribe(&self, labeler_id: Vec<u8>) -> Result<(), LabelerCatalogApiError> {
        self.labelers()
            .unsubscribe(labeler_id)
            .await
            .map(|_| ())
            .map_err(map_err)
    }

    async fn do_mint_grant(&self, grant_blob: Vec<u8>) -> Result<(), LabelerCatalogApiError> {
        CapabilitiesClient::new(self.nest.clone())
            .mint(grant_blob)
            .await
            .map(|_| ())
            .map_err(|e| seam_err(fauna_protocol::nest_seam_error(e)))
    }

    async fn do_revoke_grant(&self, grant_id: [u8; 16]) -> Result<(), LabelerCatalogApiError> {
        CapabilitiesClient::new(self.nest.clone())
            .revoke(grant_id)
            .await
            .map(|_| ())
            .map_err(|e| seam_err(fauna_protocol::nest_seam_error(e)))
    }

    async fn do_content_processor_holders(
        &self,
    ) -> Result<Vec<HolderInfo>, LabelerCatalogApiError> {
        discover_holders(&MailAdminClient::new(self.nest.clone()))
            .await
            .map_err(seam_err)
    }
}

/// The shared rejection-vs-fault classification ([`NestSeamError`]) onto this
/// page's error vocabulary — the capability deposit/revoke and the holder
/// roster read all classify through it.
fn seam_err(e: NestSeamError) -> LabelerCatalogApiError {
    match e {
        NestSeamError::Transient(detail) => LabelerCatalogApiError::Transient { detail },
        NestSeamError::Rejected(detail) => LabelerCatalogApiError::Rejected { detail },
    }
}

/// The transport-free half of [`LabelerGrantSeams`]: the owner's actor id and
/// the signer, from one keypair, over the host's stores.
fn grant_seams_from(
    keypair: &ActorKeypair,
    ledger: Arc<dyn fauna_client_config::SuccessionLedgerStore>,
    mail: Arc<dyn fauna_client_config::MailStore>,
) -> LabelerGrantSeams {
    LabelerGrantSeams {
        actor_id: keypair.actor_id().0,
        ledger,
        mail,
        signer: Arc::new(
            fauna_client_capabilities::grant_log::KeypairGrantEventSigner::new(keypair),
        ),
    }
}

fauna_core::map_rpc_error! {
    /// Map a transport `R::Error` onto [`LabelerCatalogApiError`], keyed on the
    /// WS-RPC `RpcError.code` suffix. A transport fault (the request never
    /// reached a server rejection) is `Transient`. Mirrors
    /// `fauna_devices_machine::nest_api::ws_rpc::map_err`.
    fn map_err(e) -> LabelerCatalogApiError {
        "not_found" => NotFound,
    }
}

// ── Native (`Arc<NestClient>`) ──────────────────────────────────────────────
#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::*;
    use async_trait::async_trait;
    use fauna_client::NestClient;

    #[async_trait]
    impl LabelerCatalogNestApi for WsRpcLabelerCatalogNest<Arc<NestClient>> {
        async fn list(&self) -> Result<Vec<LabelerCatalogEntry>, LabelerCatalogApiError> {
            self.do_list().await
        }
        async fn inspect(
            &self,
            labeler_id: Vec<u8>,
        ) -> Result<InspectResult, LabelerCatalogApiError> {
            self.do_inspect(labeler_id).await
        }
        async fn subscribe(
            &self,
            labeler_id: Vec<u8>,
            grant_id: Option<[u8; 16]>,
        ) -> Result<(), LabelerCatalogApiError> {
            self.do_subscribe(labeler_id, grant_id).await
        }
        async fn unsubscribe(&self, labeler_id: Vec<u8>) -> Result<(), LabelerCatalogApiError> {
            self.do_unsubscribe(labeler_id).await
        }
        async fn mint_grant(&self, grant_blob: Vec<u8>) -> Result<(), LabelerCatalogApiError> {
            self.do_mint_grant(grant_blob).await
        }
        async fn revoke_grant(&self, grant_id: [u8; 16]) -> Result<(), LabelerCatalogApiError> {
            self.do_revoke_grant(grant_id).await
        }
        async fn content_processor_holders(
            &self,
        ) -> Result<Vec<HolderInfo>, LabelerCatalogApiError> {
            self.do_content_processor_holders().await
        }
    }

    /// Build a [`LabelerCatalogMachine`] over `nest`'s authenticated WS-RPC
    /// connection **without** the grant seams: a subscribe of a `wasm` mail
    /// labeler registers the row with no grant (it never drains). Only the
    /// native apps' identity-fault fallback builds it (tui's
    /// `LabelerCatalogState::build`, linux's personalization `build_machine`:
    /// a `secret_hex` that will not decode); every construction site takes
    /// [`build_labeler_catalog_machine_with_grants`].
    pub fn build_labeler_catalog_machine(
        nest: Arc<NestClient>,
        observer: Arc<dyn LabelerCatalogObserver>,
    ) -> Arc<LabelerCatalogMachine> {
        let api: Arc<dyn LabelerCatalogNestApi> = Arc::new(WsRpcLabelerCatalogNest::new(nest));
        LabelerCatalogMachine::new(observer, api)
    }

    /// Build a [`LabelerCatalogMachine`] whose subscribe mints the per-labeler
    /// grant ([`LabelerCatalogMachine::subscribe`]): the owner's `keypair`
    /// signs the grant-log events (the same `(nest, keypair)` every minting machine is built from —
    /// `fauna_client_mail_settings::rpc_glue::build_mail_settings_machine`).
    /// The native entry tui calls. `ledger` is the host's account-store seam
    /// the grant-log events record on.
    pub fn build_labeler_catalog_machine_with_grants(
        nest: Arc<NestClient>,
        keypair: ActorKeypair,
        ledger: Arc<dyn fauna_client_config::SuccessionLedgerStore>,
        mail: Arc<dyn fauna_client_config::MailStore>,
        observer: Arc<dyn LabelerCatalogObserver>,
    ) -> Arc<LabelerCatalogMachine> {
        let seams = grant_seams_from(&keypair, ledger, mail);
        let api: Arc<dyn LabelerCatalogNestApi> = Arc::new(WsRpcLabelerCatalogNest::new(nest));
        LabelerCatalogMachine::with_grant_seams(observer, api, seams)
    }
}
#[cfg(not(target_arch = "wasm32"))]
pub use native::{build_labeler_catalog_machine, build_labeler_catalog_machine_with_grants};

// ── Wasm (`WsRpcClient`) ────────────────────────────────────────────────────
#[cfg(target_arch = "wasm32")]
mod wasm {
    use super::*;
    use async_trait::async_trait;
    use fauna_rpc_wasm::WsRpcClient;

    #[async_trait(?Send)]
    impl LabelerCatalogNestApi for WsRpcLabelerCatalogNest<WsRpcClient> {
        async fn list(&self) -> Result<Vec<LabelerCatalogEntry>, LabelerCatalogApiError> {
            self.do_list().await
        }
        async fn inspect(
            &self,
            labeler_id: Vec<u8>,
        ) -> Result<InspectResult, LabelerCatalogApiError> {
            self.do_inspect(labeler_id).await
        }
        async fn subscribe(
            &self,
            labeler_id: Vec<u8>,
            grant_id: Option<[u8; 16]>,
        ) -> Result<(), LabelerCatalogApiError> {
            self.do_subscribe(labeler_id, grant_id).await
        }
        async fn unsubscribe(&self, labeler_id: Vec<u8>) -> Result<(), LabelerCatalogApiError> {
            self.do_unsubscribe(labeler_id).await
        }
        async fn mint_grant(&self, grant_blob: Vec<u8>) -> Result<(), LabelerCatalogApiError> {
            self.do_mint_grant(grant_blob).await
        }
        async fn revoke_grant(&self, grant_id: [u8; 16]) -> Result<(), LabelerCatalogApiError> {
            self.do_revoke_grant(grant_id).await
        }
        async fn content_processor_holders(
            &self,
        ) -> Result<Vec<HolderInfo>, LabelerCatalogApiError> {
            self.do_content_processor_holders().await
        }
    }

    /// The wasm twin of the native `build_labeler_catalog_machine_with_grants`
    /// — web's only builder (`fauna-wasm-labeler-catalog`; there is no
    /// grant-less wasm twin: the SPA always holds the actor secret).
    pub fn build_labeler_catalog_machine_with_grants(
        nest: WsRpcClient,
        keypair: ActorKeypair,
        ledger: Arc<dyn fauna_client_config::SuccessionLedgerStore>,
        mail: Arc<dyn fauna_client_config::MailStore>,
        observer: Arc<dyn LabelerCatalogObserver>,
    ) -> Arc<LabelerCatalogMachine> {
        let seams = grant_seams_from(&keypair, ledger, mail);
        let api: Arc<dyn LabelerCatalogNestApi> = Arc::new(WsRpcLabelerCatalogNest::new(nest));
        LabelerCatalogMachine::with_grant_seams(observer, api, seams)
    }
}
#[cfg(target_arch = "wasm32")]
pub use wasm::build_labeler_catalog_machine_with_grants;

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer;
    use fauna_client_testkit::{CapturingRequester, block_on};
    use fauna_core::encoding::canonical_encode;
    use fauna_core::identity::ActorKeypair;
    use fauna_core::scoring::{LabelerInput, LabelerOutput, ScorerLimits};
    use fauna_protocol::ByteBuf;
    use fauna_protocol::labelers::InspectLabelerReply;
    use serde::Serialize;
    use serde::de::DeserializeOwned;

    fn seam_returning_transport_fault() -> WsRpcLabelerCatalogNest<Arc<CapturingRequester>> {
        WsRpcLabelerCatalogNest::new(Arc::new(CapturingRequester::transport_fault()))
    }

    fn seam_returning(
        code: &str,
        detail: &str,
    ) -> WsRpcLabelerCatalogNest<Arc<CapturingRequester>> {
        WsRpcLabelerCatalogNest::new(Arc::new(CapturingRequester::rejecting_code(code, detail)))
    }

    #[tokio::test]
    async fn transport_fault_maps_to_transient() {
        let r = seam_returning_transport_fault().do_list().await;
        assert!(matches!(r, Err(LabelerCatalogApiError::Transient { .. })));
    }

    #[tokio::test]
    async fn not_found_code_maps_to_not_found_variant() {
        let r = seam_returning("fauna.bridges.not_found", "no such labeler")
            .do_inspect(vec![0xAA; 32])
            .await;
        assert!(
            matches!(r, Err(LabelerCatalogApiError::NotFound { detail }) if detail == "no such labeler")
        );
    }

    fn signed_metadata(kp: &ActorKeypair, wasm_bytes: &[u8]) -> AlgorithmLabeler {
        let mut labeler = AlgorithmLabeler {
            algorithm_id: kp.actor_id(),
            version: 3,
            wasm_hash: fauna_core::encoding::content_hash(wasm_bytes),
            wasm_size: wasm_bytes.len() as u64,
            input_schema: LabelerInput {
                needs_text: true,
                needs_hashtags: true,
                needs_media_metadata: false,
                needs_author: false,
                needs_attachment_bytes: false,
            },
            output_schema: LabelerOutput::default(),
            resource_limits: ScorerLimits {
                max_memory_bytes: 16 * 1024 * 1024,
                max_cpu_microseconds: 100_000,
            },
            updated_at: fauna_core::data::Timestamp(1_700_000_000_000_000),
            signature: vec![0u8; 64],
        };
        let bytes = canonical_encode(&labeler).unwrap();
        let sig = kp.signing_key().sign(&bytes);
        labeler.signature = sig.to_bytes().to_vec();
        labeler
    }

    /// A recording requester that answers `fauna.labelers.inspect` with a
    /// real signed metadata blob, so `do_inspect`'s decode + re-verify path
    /// runs for real (not just the error-mapping tests above). `artifact_kind`
    /// rides the reply verbatim.
    #[derive(Clone)]
    struct InspectRequester {
        metadata: AlgorithmLabeler,
        wasm_bytes: Vec<u8>,
        artifact_kind: String,
    }

    impl RpcRequester for InspectRequester {
        type Error = std::convert::Infallible;
        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            _payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: Serialize,
            Reply: DeserializeOwned,
        {
            assert_eq!(kind, "fauna.labelers.inspect");
            let reply = InspectLabelerReply {
                metadata_blob: ByteBuf::from(canonical_encode(&self.metadata).unwrap()),
                wasm_bytes: ByteBuf::from(self.wasm_bytes.clone()),
                artifact_kind: self.artifact_kind.clone(),
                ..Default::default()
            };
            let bytes = fauna_protocol::encode_canonical(&reply).unwrap();
            Ok(fauna_protocol::decode_strict(&bytes).unwrap())
        }
    }

    #[test]
    fn inspect_decodes_and_verifies_a_genuine_artifact() {
        let kp = ActorKeypair::generate();
        let wasm_bytes = b"pretend wasm module bytes".to_vec();
        let metadata = signed_metadata(&kp, &wasm_bytes);
        let seam = WsRpcLabelerCatalogNest::new(InspectRequester {
            metadata: metadata.clone(),
            wasm_bytes: wasm_bytes.clone(),
            artifact_kind: "wasm".into(),
        });

        let result = block_on(seam.do_inspect(vec![0u8; 32])).expect("inspect succeeds");
        assert!(result.view.verified, "a genuine signed artifact verifies");
        assert_eq!(result.view.labeler_id, hex::encode(kp.actor_id().0));
        assert_eq!(result.view.version, 3);
        assert!(result.view.needs_text);
        assert!(result.view.needs_hashtags);
        assert_eq!(
            result.view.artifact_kind, "wasm",
            "the wire kind is transcribed verbatim"
        );
        assert!(result.view.list_name.is_none());
        assert!(result.view.list_entries.is_empty());
    }

    /// The frame's claim this machine exists to meet (content-moderation-and-
    /// ranking.md § Tier-3 artifact kinds): inspecting a `list` decodes the raw
    /// artifact into the publisher-chosen name + the exact id→score map.
    #[test]
    fn inspect_decodes_a_genuine_list_artifact_into_name_and_exact_entries() {
        let kp = ActorKeypair::generate();
        // Deliberately out of canonical order at build time — the builder owns
        // the sort; inspect must render exactly what the artifact carries.
        let bytes = fauna_core::scoring::build_list_artifact(
            Some("Small orange cats"),
            vec![([0x22u8; 32], 900), ([0x11u8; 32], 400)],
        )
        .unwrap();
        let metadata = signed_metadata(&kp, &bytes);
        let seam = WsRpcLabelerCatalogNest::new(InspectRequester {
            metadata,
            wasm_bytes: bytes,
            artifact_kind: "list".into(),
        });

        let result = block_on(seam.do_inspect(vec![0u8; 32])).expect("inspect succeeds");
        assert!(result.view.verified, "a genuine signed List verifies");
        assert_eq!(result.view.artifact_kind, "list");
        assert_eq!(result.view.list_name.as_deref(), Some("Small orange cats"));
        let entries: Vec<(String, i64)> = result
            .view
            .list_entries
            .iter()
            .map(|e| (e.content_id.clone(), e.score))
            .collect();
        assert_eq!(
            entries,
            vec![
                (hex::encode([0x11u8; 32]), 400),
                (hex::encode([0x22u8; 32]), 900),
            ],
            "the EXACT map, in the artifact's canonical ascending order"
        );
    }

    /// A `list` whose bytes don't decode as a List artifact must NOT read as
    /// verified — even when the metadata's hash/size/signature bind those very
    /// bytes (a signed blob of garbage). What cannot be inspected cannot be
    /// trusted, and `verified` is what gates subscribe.
    #[test]
    fn inspect_flags_an_undecodable_list_as_unverified() {
        let kp = ActorKeypair::generate();
        let garbage = b"not a dag-cbor list artifact".to_vec();
        // The metadata binds the garbage honestly, so the signature check alone
        // would pass — the list-decode fold is what must catch this.
        let metadata = signed_metadata(&kp, &garbage);
        let seam = WsRpcLabelerCatalogNest::new(InspectRequester {
            metadata,
            wasm_bytes: garbage,
            artifact_kind: "list".into(),
        });

        let result = block_on(seam.do_inspect(vec![0u8; 32])).expect("inspect still answers");
        assert!(
            !result.view.verified,
            "an undecodable List must not read as verified"
        );
        assert!(result.view.list_name.is_none());
        assert!(result.view.list_entries.is_empty());
    }

    #[test]
    fn inspect_flags_a_tampered_artifact_as_unverified() {
        let kp = ActorKeypair::generate();
        let wasm_bytes = b"pretend wasm module bytes".to_vec();
        let metadata = signed_metadata(&kp, &wasm_bytes);
        // Serve DIFFERENT bytes than what the metadata's hash/size describe —
        // simulating a compromised/lying nest swapping the module.
        let seam = WsRpcLabelerCatalogNest::new(InspectRequester {
            metadata,
            wasm_bytes: b"a completely different, swapped module".to_vec(),
            artifact_kind: String::new(),
        });

        let result = block_on(seam.do_inspect(vec![0u8; 32])).expect("inspect still decodes");
        assert!(
            !result.view.verified,
            "hash/size mismatch must NOT read as verified"
        );
    }
}
