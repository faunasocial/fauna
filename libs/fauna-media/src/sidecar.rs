//! UploadSidecar wire shape (canonical DAG-CBOR via `fauna-cbor`).
//!
//! Carries the four fields the nest's `blob_metadata` table needs (mime,
//! thumbnail-blob hash, has_c2pa) plus the AudienceClass tag the nest's
//! eventual strict-verifier reads to pick which AEAD envelope shape to
//! verify. Audience binding identifiers (group_id, post_id, etc.) are NOT
//! in the sidecar — they live on the referencing content per the goal
//! doc's Media row.

use serde::{Deserialize, Serialize};

use crate::audience::AudienceClass;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadSidecar {
    /// The dispatch tag the nest's strict-verifier reads to pick the
    /// envelope shape to verify.
    pub class: AudienceClass,
    /// Uploader-asserted MIME (matches `blob_metadata.mime` column,
    /// served as `Content-Type` on download).
    pub mime: String,
    /// Uploader-asserted C2PA presence (matches the `x-c2pa` response header).
    pub has_c2pa: bool,
    /// BLAKE3 hash of a separate sealed thumbnail blob (sealed under the
    /// same audience as the parent). Absent when no thumbnail was generated.
    #[serde(default, with = "serde_bytes")]
    pub thumbnail_hash: Option<[u8; 32]>,
}

impl UploadSidecar {
    /// Canonical DAG-CBOR encoding — the bytes that ride in the `sidecar`
    /// multipart part of `POST /api/v1/blob`. Every app encodes through this
    /// one helper so the wire bytes are byte-identical across the six apps
    /// (priority #1). The nest decodes the mirror with
    /// [`UploadSidecar::from_dag_cbor`] (see
    /// `bins/fauna-nest/src/blob_routes.rs::parse_multipart_upload`).
    pub fn to_dag_cbor(&self) -> Vec<u8> {
        fauna_cbor::encode_canonical(self)
            .expect("UploadSidecar serialization is infallible into a Vec")
    }

    /// Strict canonical-DAG-CBOR decode — rejects any non-canonical sidecar
    /// before deserializing. Mirror of [`UploadSidecar::to_dag_cbor`].
    pub fn from_dag_cbor(bytes: &[u8]) -> Result<Self, fauna_cbor::DecodeError> {
        fauna_cbor::decode_strict(bytes)
    }

    /// The sidecar for a gated post's **already-sealed** full-body blob — the one
    /// shared source every app's gated upload builds from, so the
    /// `PeriodRestrictedPost` sidecar is byte-identical across all apps
    /// (priority #1/#2). The sealed bytes come from the shared post builder
    /// (`fauna_client_core::post::build_gated_post`, sealed via
    /// `encrypt_content(derive_post_key(period_key, seal_id), …)`); this is
    /// transport glue only — no `process_and_seal`, so the real MIME rides inside
    /// the seal and the sidecar mime is `application/octet-stream` with no
    /// thumbnail (`ui/feed.md` § Encryption at rest). Callers:
    /// `fauna_client::upload_gated_post_blob` (native HTTP), `fauna_wasm::gated_post_sidecar`
    /// (web), `fauna_ffi::gated_post_sidecar` (Apple / Windows / Android).
    pub fn gated_post() -> Self {
        UploadSidecar {
            class: AudienceClass::PeriodRestrictedPost,
            mime: "application/octet-stream".to_string(),
            has_c2pa: false,
            thumbnail_hash: None,
        }
    }

    /// The sidecar for a **room-restricted** post's already-sealed full-body
    /// blob — [`Self::gated_post`]'s twin for the room arm. The class is the
    /// one the post's attachments already declare, `GroupRestrictedPost`
    /// (`ui/feed.md` § Encryption at rest → *Room-restricted — the ruling*,
    /// ruling 4: no new sidecar class), so a room post's body and its photos
    /// name one audience. Chosen by
    /// `FeedManager::gated_upload_sidecar` off the staged post, never by an app.
    pub fn room_post() -> Self {
        UploadSidecar {
            class: AudienceClass::GroupRestrictedPost,
            ..Self::gated_post()
        }
    }

    /// The sidecar for a conversation attachment's **already-sealed** blob —
    /// the bytes come from `FaunaMlsBackend::send`'s
    /// `engine.seal_conversation_blob(channel_id, &att.bytes)` (sealed under
    /// the channel's `derive_blob_key(epoch_secret)`), so — like
    /// [`Self::gated_post`] — this is transport glue only: no
    /// `process_and_seal`, the real MIME rides inside the seal, and the
    /// sidecar mime is `application/octet-stream` with no thumbnail
    /// (`docs/goal/ui/conversations.md` § Attachments).
    pub fn conversation_attachment() -> Self {
        UploadSidecar {
            class: AudienceClass::Conversation,
            mime: "application/octet-stream".to_string(),
            has_c2pa: false,
            thumbnail_hash: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dag_cbor_round_trips() {
        let sidecar = UploadSidecar {
            class: AudienceClass::PublicPost,
            mime: "image/png".to_string(),
            has_c2pa: false,
            thumbnail_hash: Some([7u8; 32]),
        };
        let bytes = sidecar.to_dag_cbor();
        let decoded = UploadSidecar::from_dag_cbor(&bytes).unwrap();
        assert_eq!(decoded, sidecar);
    }
}
