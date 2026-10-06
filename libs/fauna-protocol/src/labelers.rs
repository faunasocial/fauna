//! WS-RPC payload types for the community-labeler registry (`fauna.labelers.*`).
//!
//! A publisher registers a signed, versioned, size-&-resource-bounded WASM
//! labeler artifact (`publish`); a user browses the catalog (`list`), inspects
//! one labeler's full record + WASM bytes before trusting it (`inspect`), and
//! records a revocable subscription that registers the labeler's version so the
//! re-score drain owes a re-score (`subscribe`/`unsubscribe`). Kind registry
//! entries live in `kind.rs::register_labeler_kinds` (the client-facing twin of
//! the nest's `register_labeler_handlers`).
//!
//! Design tracked internally. The metadata itself is a canonical-CBOR
//! `fauna_core::scoring::AlgorithmLabeler` carried opaque in `metadata_blob`;
//! the summary columns below are projections of it (indexed for `list`), the way
//! `capability_grants` indexes `holder_pubkey`/`epoch_end` beside its opaque
//! blob.

use crate::Value;
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use std::collections::BTreeMap;

/// `fauna.labelers.publish` — a publisher (any authenticated actor) deposits a
/// signed `AlgorithmLabeler` (`metadata_blob`, canonical CBOR) plus its WASM
/// module (`wasm_bytes`). The nest verifies the signature against the labeler's
/// `algorithm_id`, checks `wasm_hash`/`wasm_size`, validates the module compiles
/// under the sandbox, and stores it iff `version` strictly increases (monotonic).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PublishLabelerRequest {
    /// Canonical-CBOR `fauna_core::scoring::AlgorithmLabeler` (opaque here;
    /// decoded + signature-verified by the nest handler).
    pub metadata_blob: ByteBuf,
    /// The WASM module bytes (`≤ MAX_LABELER_WASM_BYTES`), hashed to
    /// `metadata.wasm_hash` and compile-validated at publish.
    pub wasm_bytes: ByteBuf,
    /// The `content.read{kind}` universe this labeler scores (`post` | `mail`;
    /// design § 6 + Slice-3 D7). Every publisher names it — the nest refuses an
    /// empty kind as `malformed`, like `artifact_kind`. It is
    /// deliberately **not** a field of the signed `AlgorithmLabeler` metadata
    /// (design § 3 keeps that shape frozen, so the `label()` ABI stays
    /// post-shaped); it is a routing declaration the publisher makes and
    /// subscribers see via `list`/`inspect` before minting a matching
    /// capability. The nest validates it and projects it into
    /// `labelers.content_kind`.
    #[serde(default)]
    pub content_kind: String,
    /// The artifact kind (`wasm` | `list` | `text-model`; design Block A, D8).
    /// Every publisher names it — the nest refuses an empty kind as
    /// `malformed`. A `list` artifact carries a dag-cbor
    /// `fauna_core::scoring::LabelerListArtifact` in `wasm_bytes` (the field
    /// name is kept for in-major wire compat — read it as "artifact bytes");
    /// the `wasm_hash`/`wasm_size` metadata binding and signature verify are
    /// identical for both kinds. Like `content_kind`, it is deliberately not a
    /// field of the signed `AlgorithmLabeler` (that shape stays frozen).
    #[serde(default)]
    pub artifact_kind: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PublishLabelerReply {
    /// The stored labeler's id (`AlgorithmLabeler.algorithm_id`, 32 bytes).
    pub labeler_id: ByteBuf,
    /// The version now stored (echoed from the accepted metadata).
    pub version: u64,
    pub ok: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One catalog entry, metadata-only (**no WASM bytes** — the user fetches those
/// with `inspect` before trusting the module). Projections of the stored
/// `AlgorithmLabeler` the `list` browse surface renders.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LabelerSummary {
    pub labeler_id: ByteBuf,
    pub version: u64,
    /// The actor that published (== the artifact signer).
    pub publisher_actor: ByteBuf,
    /// The `content.read{kind}` this labeler scores (`post` | `mail` | …).
    pub content_kind: String,
    /// The `content_scores.factor` it writes (`labeler:<hex>`; design § 4).
    pub factor: String,
    /// Content hash of the WASM module (36-byte `ContentHash`).
    pub wasm_hash: ByteBuf,
    pub wasm_size: u64,
    /// The artifact kind (`wasm` | `list` | `text-model`; design Block A, D8),
    /// as the publisher declared it (never empty).
    #[serde(default)]
    pub artifact_kind: String,
    /// For a `text-model` artifact: its tokenizer/schema `version` — the
    /// contract a subscriber's scorer must implement
    /// (`fauna_core::scoring::TEXT_MODEL_ARTIFACT_VERSION`). Read off the
    /// artifact by the nest at its publish gate, which already decodes it.
    ///
    /// This exists so the **metadata-only** browse can answer "can this build
    /// score it?" *before* inspect: the version lives inside the artifact bytes,
    /// which only `inspect` returns, and a browse surface must not cost
    /// N × 64 KiB round trips to render a badge
    /// (`content-moderation-and-ranking.md` § Tier-3 artifact kinds — the
    /// unknown-version contract's "and says so" half).
    ///
    /// Additive (`#[serde(default)]`): `0` means *absent or not applicable* — every
    /// non-`text-model` artifact.
    /// A client reads `0` as "no version claim", which renders exactly today's
    /// behaviour, so this is additive-everywhere with no compat break
    /// (`version-compatibility.md`).
    #[serde(default)]
    pub artifact_version: u64,
    /// Whether the **caller** currently subscribes to this labeler
    /// (`labeler_subscriptions` row keyed on the requesting actor). Additive
    /// (`#[serde(default)]`) — an absent key decodes as `false`, the safe default (the personalization home's
    /// subscribed-labelers facet then just shows fewer rows, never a false
    /// "subscribed"). content-moderation-and-ranking.md § Tier-3.
    #[serde(default)]
    pub subscribed: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.labelers.list` — browse the catalog (metadata only, no bytes).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ListLabelersRequest {
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ListLabelersReply {
    pub labelers: Vec<LabelerSummary>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.labelers.inspect` — fetch one labeler's full record **+ WASM bytes**
/// (inspect-before-subscribe transparency: the user reads the exact module
/// before granting it their sealed content).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct InspectLabelerRequest {
    pub labeler_id: ByteBuf,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct InspectLabelerReply {
    /// Canonical-CBOR `AlgorithmLabeler` (carries `input_schema`,
    /// `resource_limits`, `signature`).
    pub metadata_blob: ByteBuf,
    /// The full artifact bytes — the WASM module for `artifact_kind == "wasm"`,
    /// the dag-cbor `LabelerListArtifact` for `"list"`, the dag-cbor
    /// `TextModelArtifact` for `"text-model"` (the field name is kept for
    /// in-major wire compat).
    pub wasm_bytes: ByteBuf,
    /// The artifact kind (`wasm` | `list` | `text-model`; design Block A, D8),
    /// as the publisher declared it (never empty). Tells the client how to render
    /// the bytes for inspect-before-subscribe (a `list` renders the exact
    /// id→score map; a `text-model` renders its full vocabulary) — and, for
    /// `text-model`, how to *score* with them, since that kind is evaluated
    /// client-side.
    #[serde(default)]
    pub artifact_kind: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.labelers.subscribe` (owner == caller) — record a subscription and
/// register the labeler's version so the existing re-score obligation scan owes
/// a re-score for the owner's content. `grant_id` links the one capability grant
/// the client minted for a restricted kind (so the settings page shows one
/// entry); `None` for a public-only subscription (no grant needed).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SubscribeLabelerRequest {
    pub labeler_id: ByteBuf,
    /// `capability_grants.grant_id` (16 bytes) for a restricted-kind
    /// subscription, or `None` for public-only.
    pub grant_id: Option<ByteBuf>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SubscribeLabelerReply {
    /// The bus factor the subscription registered (`labeler:<hex>`; design § 4).
    pub factor: String,
    pub ok: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.labelers.unsubscribe` (owner == caller) — drop the subscription
/// (idempotent). The client separately revokes the capability grant via
/// `fauna.capabilities.revoke`, taking the drain dark for that owner.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UnsubscribeLabelerRequest {
    pub labeler_id: ByteBuf,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UnsubscribeLabelerReply {
    pub ok: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict as decode, encode_canonical};

    #[test]
    fn labeler_registry_rpc_round_trips() {
        let pub_req = PublishLabelerRequest {
            metadata_blob: ByteBuf::from(vec![0x11; 64]),
            wasm_bytes: ByteBuf::from(vec![0x22; 128]),
            // A declared non-default kind must survive the round-trip (D7),
            // as must a non-default artifact kind (Block A, D8).
            content_kind: "mail".into(),
            artifact_kind: "list".into(),
            ..Default::default()
        };
        let bytes = encode_canonical(&pub_req).unwrap();
        let decoded_pub = decode::<PublishLabelerRequest>(&bytes).unwrap();
        assert_eq!(decoded_pub, pub_req);
        assert_eq!(decoded_pub.content_kind, "mail");
        assert_eq!(decoded_pub.artifact_kind, "list");

        let pub_reply = PublishLabelerReply {
            labeler_id: ByteBuf::from(vec![0x33; 32]),
            version: 7,
            ok: true,
            ..Default::default()
        };
        let bytes = encode_canonical(&pub_reply).unwrap();
        assert_eq!(decode::<PublishLabelerReply>(&bytes).unwrap(), pub_reply);

        let list_req = ListLabelersRequest::default();
        let bytes = encode_canonical(&list_req).unwrap();
        assert_eq!(decode::<ListLabelersRequest>(&bytes).unwrap(), list_req);

        let list_reply = ListLabelersReply {
            labelers: vec![LabelerSummary {
                labeler_id: ByteBuf::from(vec![0x44; 32]),
                version: 2,
                publisher_actor: ByteBuf::from(vec![0x44; 32]),
                content_kind: "post".into(),
                factor: "labeler:deadbeef".into(),
                wasm_hash: ByteBuf::from(vec![0x55; 36]),
                wasm_size: 128,
                artifact_kind: "list".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let bytes = encode_canonical(&list_reply).unwrap();
        assert_eq!(decode::<ListLabelersReply>(&bytes).unwrap(), list_reply);

        let inspect_req = InspectLabelerRequest {
            labeler_id: ByteBuf::from(vec![0x44; 32]),
            ..Default::default()
        };
        let bytes = encode_canonical(&inspect_req).unwrap();
        assert_eq!(
            decode::<InspectLabelerRequest>(&bytes).unwrap(),
            inspect_req
        );

        let inspect_reply = InspectLabelerReply {
            metadata_blob: ByteBuf::from(vec![0x66; 64]),
            wasm_bytes: ByteBuf::from(vec![0x77; 128]),
            artifact_kind: "wasm".into(),
            ..Default::default()
        };
        let bytes = encode_canonical(&inspect_reply).unwrap();
        assert_eq!(
            decode::<InspectLabelerReply>(&bytes).unwrap(),
            inspect_reply
        );

        // subscribe with a grant, and public-only (grant_id None)
        let sub_req = SubscribeLabelerRequest {
            labeler_id: ByteBuf::from(vec![0x44; 32]),
            grant_id: Some(ByteBuf::from(vec![0x88; 16])),
            ..Default::default()
        };
        let bytes = encode_canonical(&sub_req).unwrap();
        assert_eq!(decode::<SubscribeLabelerRequest>(&bytes).unwrap(), sub_req);

        let sub_req_public = SubscribeLabelerRequest {
            labeler_id: ByteBuf::from(vec![0x44; 32]),
            grant_id: None,
            ..Default::default()
        };
        let bytes = encode_canonical(&sub_req_public).unwrap();
        assert_eq!(
            decode::<SubscribeLabelerRequest>(&bytes).unwrap(),
            sub_req_public
        );

        let sub_reply = SubscribeLabelerReply {
            factor: "labeler:deadbeef".into(),
            ok: true,
            ..Default::default()
        };
        let bytes = encode_canonical(&sub_reply).unwrap();
        assert_eq!(decode::<SubscribeLabelerReply>(&bytes).unwrap(), sub_reply);

        let unsub_req = UnsubscribeLabelerRequest {
            labeler_id: ByteBuf::from(vec![0x44; 32]),
            ..Default::default()
        };
        let bytes = encode_canonical(&unsub_req).unwrap();
        assert_eq!(
            decode::<UnsubscribeLabelerRequest>(&bytes).unwrap(),
            unsub_req
        );

        let unsub_reply = UnsubscribeLabelerReply {
            ok: true,
            ..Default::default()
        };
        let bytes = encode_canonical(&unsub_reply).unwrap();
        assert_eq!(
            decode::<UnsubscribeLabelerReply>(&bytes).unwrap(),
            unsub_reply
        );
    }

    #[test]
    fn publish_request_preserves_unknown_fields() {
        // A newer peer sends an extra key; it must round-trip through `extra`
        // (transport.md § forward-compat discipline, rule 4).
        let mut with_extra = PublishLabelerRequest {
            metadata_blob: ByteBuf::from(vec![0xCD; 4]),
            wasm_bytes: ByteBuf::from(vec![0xEF; 4]),
            ..Default::default()
        };
        with_extra
            .extra
            .insert("future_field".into(), Value::Integer(11.into()));
        let bytes = encode_canonical(&with_extra).unwrap();
        let decoded: PublishLabelerRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, with_extra);
        assert_eq!(
            decoded.extra.get("future_field"),
            Some(&Value::Integer(11.into()))
        );
    }

    #[test]
    fn subscribe_reply_preserves_unknown_fields() {
        let mut with_extra = SubscribeLabelerReply {
            factor: "labeler:cafe".into(),
            ok: true,
            ..Default::default()
        };
        with_extra
            .extra
            .insert("future_field".into(), Value::Integer(5.into()));
        let bytes = encode_canonical(&with_extra).unwrap();
        let decoded: SubscribeLabelerReply = decode(&bytes).unwrap();
        assert_eq!(decoded, with_extra);
        assert_eq!(
            decoded.extra.get("future_field"),
            Some(&Value::Integer(5.into()))
        );
    }
}
