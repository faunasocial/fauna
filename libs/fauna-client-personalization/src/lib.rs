//! Typed-call wrapper for the `fauna.personalization.model.*` WS-RPC kinds,
//! plus the **seal** of the blob those kinds carry
//! (`docs/goal/behavior/topic-factors.md` § Wire & registry + § At rest).
//!
//! Two halves, deliberately in one crate — the wire and the seal of the blob it
//! carries:
//!
//! * [`PersonalizationClient`] — one thin async method per kind, generic over
//!   the [`RpcRequester`] transport, no state machine.
//! * [`seal_topic_model`] / [`unseal_topic_model`] — the at-rest crypto. They
//!   live *here*, beside the wire, and not in `fauna-text-model`, because that
//!   crate is deliberately dependency-light (serde + unicode, no crypto): the
//!   model knows how to *be* a model, this crate knows how it travels. Both are
//!   thin skins over the kind-agnostic [`seal_model_bytes`] /
//!   [`unseal_model_bytes`] byte pipeline, which `fauna-feed`'s engagement-cue
//!   rollup also seals through — one pipeline, not one per sealed model type.
//!
//! # The blob is nest-opaque from birth
//!
//! Unlike the spam model there is **no server-side train path at any point in
//! the lifecycle** (§ Placement), so there is no lazy seal-on-first-write, no
//! fail-closed guard, and no seal-vs-server-write race to reason about: the
//! client seals before the first `put` and the nest stores bytes it can never
//! read. It never holds the seal key.
//!
//! # The seal key: the delegable unit for one reserved kind
//!
//! Every blob seals under [`model_seal_keys`] — the delegable schedule's
//! per-kind pair for [`MODEL_SEAL_KIND`] (`fauna.personalization.model`),
//! rooted in the owner's [`BackupKey`] so every device derives it. Not the
//! BackupKey itself: that key also seals backups, drafts, library media and
//! single-user folder chunks, so no grant may ever wrap it, and the
//! `fauna:personalization:rw` scope needs a grant twin
//! (§ At rest → *Re-keyed for the third-party plane*). The pair is that twin:
//! a grantee rebuilds it with `AccountStateKindKeys::from_grant` and opens the
//! same blobs through the same functions. Not the MSEK-derived recipient seal
//! the spam model uses either: the MSEK exists only when mail is enabled, and a
//! personalization feature must work for a mail-less actor.
//!
//! # Seal pipeline
//!
//! ```text
//! seal:    TopicModel::to_bytes  →  compress_chunk (zstd)  →  encrypt_kind_chunk (ChaCha20-Poly1305)
//! unseal:  decrypt_kind_chunk    →  decompress_chunk       →  TopicModel::from_bytes
//! ```

pub mod publish;
pub mod topics;
pub use publish::{
    PublishListError, PublishedList, labeler_signing_seed, publish_trained_factor_list,
};
pub use topics::{TrainedTopicRow, TrainedTopics, TrainedTopicsError};

use fauna_core::crypto::{
    AccountStateKindKeys, BackupKey, DelegableKindKeys, DelegableSchedule, seal_kind_chunk,
    unseal_kind_chunk,
};
use fauna_protocol::RpcRequester;
use fauna_protocol::personalization::{
    KIND_MODEL_DELETE, KIND_MODEL_FETCH, KIND_MODEL_PUT, MODEL_SEAL_KIND,
    PersonalizationModelDeleteReply, PersonalizationModelDeleteRequest,
    PersonalizationModelFetchReply, PersonalizationModelFetchRequest, PersonalizationModelPutReply,
    PersonalizationModelPutRequest,
};
use fauna_text_model::topic::{TOPIC_MODEL_MAX_BYTES, TopicModel};
use serde_bytes::ByteBuf;

pub use fauna_core::crypto::AccountStateKindKeys as ModelSealKey;
pub use fauna_protocol::personalization as wire;

/// A sealed model blob failed to open, or a model failed to seal.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ModelSealError {
    /// Wrong key, or a tampered / truncated blob.
    #[error("sealed model failed to decrypt: {0}")]
    Decrypt(String),
    /// The plaintext is not a valid zstd chunk.
    #[error("sealed model failed to decompress: {0}")]
    Decompress(String),
    /// The plaintext decompressed but did not decode to the expected model type
    /// (a `TopicModel`, or a `CueRollup` at the cue call site) — a blob written by
    /// a *newer* client with an incompatible layout, or corruption. The byte-level
    /// [`seal_model_bytes`] / [`unseal_model_bytes`] never raise this: decoding
    /// the plaintext to a concrete type is the caller's step, so the caller maps
    /// its own decode failure to this variant.
    #[error("sealed plaintext did not decode to the expected model")]
    Decode,
    /// The AEAD seal itself failed (should not happen for a well-formed key).
    #[error("model failed to encrypt: {0}")]
    Encrypt(String),
}

/// The seal keys of every personalization blob: the delegable schedule's pair
/// for [`MODEL_SEAL_KIND`], derived from the owner's [`BackupKey`]. The returned
/// [`DelegableKindKeys`] derefs to the [`ModelSealKey`] the seal functions take,
/// and its `to_grant` is the `fauna:personalization:rw` grant twin.
pub fn model_seal_keys(backup_key: &BackupKey) -> DelegableKindKeys {
    DelegableSchedule::derive(backup_key).for_kind(MODEL_SEAL_KIND)
}

/// The shared model byte-seal: `compress_chunk` (zstd) then
/// `encrypt_kind_chunk` (ChaCha20-Poly1305) under [`model_seal_keys`].
/// Kind-agnostic — it seals whatever canonical bytes it is handed, so both the
/// trained-topic model ([`seal_topic_model`]) and the engagement-cue rollup
/// (`fauna_feed::cues::seal_cue_rollup`) ride the identical pipeline rather than
/// each open-coding it (priority #2/#4). The **caller** produces the canonical
/// bytes and enforces any at-rest size cap first — the seal cannot cap a shape it
/// cannot interpret.
pub fn seal_model_bytes(
    plaintext: &[u8],
    keys: &AccountStateKindKeys,
) -> Result<Vec<u8>, ModelSealError> {
    seal_kind_chunk(plaintext, keys, ModelSealError::Encrypt)
}

/// Open bytes sealed by [`seal_model_bytes`], returning the canonical plaintext
/// — the caller decodes it to its concrete type (and maps a decode failure to
/// [`ModelSealError::Decode`]). Errors are **not** collapsed into "absent": a
/// blob that exists but will not open is a real fault (wrong key, corruption, a
/// newer layout), and silently treating it as fresh would discard everything the
/// user's other devices recorded, then overwrite it on the next `put`.
pub fn unseal_model_bytes(
    blob: &[u8],
    keys: &AccountStateKindKeys,
) -> Result<Vec<u8>, ModelSealError> {
    unseal_kind_chunk(
        blob,
        keys,
        ModelSealError::Decrypt,
        ModelSealError::Decompress,
    )
}

/// Seal a trained topic model under [`model_seal_keys`] — the exact bytes
/// [`PersonalizationClient::model_put`] sends and the nest stores verbatim.
///
/// The model is **capped to [`TOPIC_MODEL_MAX_BYTES`] first**. The seal is
/// opaque to the nest, so the client is the only place the at-rest bound can be
/// enforced (the spam model's `cap_to_bytes`-before-seal discipline,
/// `mail-spam.md` § Bounded size); a caller that skipped the cap would only
/// discover the overflow as a `put` rejection it cannot repair. Capping here
/// makes that unrepresentable. The seal itself is [`seal_model_bytes`].
pub fn seal_topic_model(
    model: &TopicModel,
    keys: &AccountStateKindKeys,
) -> Result<Vec<u8>, ModelSealError> {
    let mut capped = model.clone();
    capped.cap_to_bytes(TOPIC_MODEL_MAX_BYTES);
    seal_model_bytes(&capped.to_bytes(), keys)
}

/// Open a sealed model blob — the inverse of [`seal_topic_model`], decoding the
/// [`unseal_model_bytes`] plaintext back into a [`TopicModel`]. A blob that
/// opens but does not decode (a newer layout, corruption) is
/// [`ModelSealError::Decode`], never a silent empty model (see
/// [`unseal_model_bytes`]).
pub fn unseal_topic_model(
    blob: &[u8],
    keys: &AccountStateKindKeys,
) -> Result<TopicModel, ModelSealError> {
    let plaintext = unseal_model_bytes(blob, keys)?;
    TopicModel::from_bytes(&plaintext).ok_or(ModelSealError::Decode)
}

/// Typed `fauna.personalization.model.*` call surface, generic over the WS-RPC
/// transport (`R: RpcRequester`): native call sites pass `Arc<NestClient>`, the
/// wasm SPA passes its `WsRpcClient`.
///
/// Every kind is **User-class and owner-scoped** — the nest derives the actor
/// from the authenticated connection, so no request carries an `actor_id` and a
/// caller can only ever touch its own rows.
pub struct PersonalizationClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> PersonalizationClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.personalization.model.fetch` — the caller's sealed blob for
    /// `factor`. An absent row (never trained on any device, or deleted) comes
    /// back as `sealed_blob: None`, which the caller reads as "start from a
    /// fresh empty model" — *not* as an error.
    pub async fn model_fetch(
        &self,
        factor: impl Into<String>,
    ) -> Result<PersonalizationModelFetchReply, R::Error> {
        self.nest
            .request(
                KIND_MODEL_FETCH,
                PersonalizationModelFetchRequest {
                    factor: factor.into(),
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.personalization.model.put` — persist the sealed blob for
    /// `factor` (idempotent overwrite; a concurrent two-device put is
    /// last-put-wins, § Placement). `sample_count` is **advisory** metadata for
    /// the settings display and the adopt-if-larger reconcile hint — the nest
    /// stores it verbatim and can never validate it against the opaque blob.
    pub async fn model_put(
        &self,
        factor: impl Into<String>,
        sealed_blob: Vec<u8>,
        sample_count: u32,
    ) -> Result<PersonalizationModelPutReply, R::Error> {
        self.nest
            .request(
                KIND_MODEL_PUT,
                PersonalizationModelPutRequest {
                    factor: factor.into(),
                    sealed_blob: ByteBuf::from(sealed_blob),
                    sample_count,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.personalization.model.delete` — drop the caller's row for
    /// `factor`. Idempotent (`deleted: false` when no row existed). This is the
    /// user's own destruction of their own revocable data (§ At rest); the
    /// registry entry is removed separately, through `fauna.state.personalization`.
    pub async fn model_delete(
        &self,
        factor: impl Into<String>,
    ) -> Result<PersonalizationModelDeleteReply, R::Error> {
        self.nest
            .request(
                KIND_MODEL_DELETE,
                PersonalizationModelDeleteRequest {
                    factor: factor.into(),
                    extra: Default::default(),
                },
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_text_model::topic::ExampleLabel;

    fn key() -> DelegableKindKeys {
        model_seal_keys(&BackupKey::derive(&[7u8; 32]))
    }

    fn trained() -> TopicModel {
        let mut m = TopicModel::new();
        m.train("aa", "cats are soft and warm", ExampleLabel::MoreLikeThis);
        m.train(
            "bb",
            "quarterly earnings report",
            ExampleLabel::LessLikeThis,
        );
        m
    }

    #[test]
    fn seal_then_unseal_round_trips_the_model() {
        let m = trained();
        let blob = seal_topic_model(&m, &key()).unwrap();
        assert_eq!(unseal_topic_model(&blob, &key()).unwrap(), m);
    }

    /// The seal is a privacy boundary: the sealed bytes must not leak the
    /// trained vocabulary the nest is never allowed to learn.
    #[test]
    fn the_sealed_blob_does_not_contain_the_plaintext_vocabulary() {
        let blob = seal_topic_model(&trained(), &key()).unwrap();
        let haystack = String::from_utf8_lossy(&blob);
        for token in ["cats", "earnings", "soft"] {
            assert!(
                !haystack.contains(token),
                "sealed blob leaked the trained token {token:?}",
            );
        }
    }

    #[test]
    fn a_wrong_key_fails_loudly_rather_than_reading_as_empty() {
        let blob = seal_topic_model(&trained(), &key()).unwrap();
        let wrong = model_seal_keys(&BackupKey::derive(&[9u8; 32]));
        // Not `Ok(TopicModel::new())` — an unopenable blob must never be
        // mistaken for "never trained", which would train over the user's real
        // model on the next put.
        assert!(matches!(
            unseal_topic_model(&blob, &wrong),
            Err(ModelSealError::Decrypt(_))
        ));
    }

    #[test]
    fn a_truncated_blob_fails_loudly() {
        let blob = seal_topic_model(&trained(), &key()).unwrap();
        let truncated = &blob[..blob.len() / 2];
        assert!(unseal_topic_model(truncated, &key()).is_err());
    }

    /// Every device derives the same seal keys from the identity seed, so a
    /// model sealed on one device opens on another (§ At rest — cross-device
    /// sync is fetch-on-demand of this very blob).
    #[test]
    fn a_second_device_with_the_same_seed_opens_the_blob() {
        let seed = [42u8; 32];
        let device_a = model_seal_keys(&BackupKey::derive(&seed));
        let device_b = model_seal_keys(&BackupKey::derive(&seed));
        let blob = seal_topic_model(&trained(), &device_a).unwrap();
        assert_eq!(unseal_topic_model(&blob, &device_b).unwrap(), trained());
    }

    /// The at-rest cap is enforced at the seal, the only place that can (the
    /// nest sees opaque bytes). A model far over the bound is shed down before
    /// it is ever sent, rather than bouncing off the nest's 512 KiB `put`
    /// rejection with no way for the caller to repair it.
    #[test]
    fn seal_caps_an_oversized_model_before_it_reaches_the_wire() {
        let mut m = TopicModel::new();
        for i in 0..4000 {
            m.train(
                &format!("{i:04x}"),
                &format!("distinct vocabulary token number {i} filling the ngram table"),
                ExampleLabel::MoreLikeThis,
            );
        }
        assert!(
            m.to_bytes().len() > TOPIC_MODEL_MAX_BYTES,
            "fixture must actually exceed the cap to test the cap",
        );
        let blob = seal_topic_model(&m, &key()).unwrap();
        let reopened = unseal_topic_model(&blob, &key()).unwrap();
        assert!(
            reopened.to_bytes().len() <= TOPIC_MODEL_MAX_BYTES,
            "the sealed model must respect the at-rest cap",
        );
        // Capping sheds n-grams/markers, never the document counters — the
        // damp's sample count survives (`TopicModel::cap_to_bytes`).
        assert_eq!(reopened.example_count(), m.example_count());
    }

    /// The seal key is the delegable unit for the reserved kind — exactly the
    /// pair `DelegableSchedule::for_kind(MODEL_SEAL_KIND)` derives, so it is the
    /// `fauna:personalization:rw` grant twin (§ At rest → *Re-keyed for the
    /// third-party plane*): a grantee rebuilding the pair from the grant opens
    /// the owner's blob, and the blob carries the kind-keyed form byte.
    #[test]
    fn the_seal_key_is_the_reserved_kinds_delegable_unit_and_its_grant_opens_the_blob() {
        assert_eq!(MODEL_SEAL_KIND, "fauna.personalization.model");
        let backup = BackupKey::derive(&[7u8; 32]);
        let expected = DelegableSchedule::derive(&backup).for_kind(MODEL_SEAL_KIND);
        let keys = model_seal_keys(&backup);
        assert_eq!(keys.to_grant(), expected.to_grant());
        assert_eq!(keys.kind(), MODEL_SEAL_KIND);

        let blob = seal_topic_model(&trained(), &keys).unwrap();
        assert_eq!(blob[0], fauna_core::crypto::KIND_CHUNK_FORM);

        let (entry_key, item_blind) = keys.to_grant();
        let grantee = AccountStateKindKeys::from_grant(MODEL_SEAL_KIND, entry_key, item_blind);
        assert_eq!(unseal_topic_model(&blob, &grantee).unwrap(), trained());
    }

    /// The re-seal is an in-place cutover: a blob in the BackupKey's own chunk
    /// form is refused, never read — and so is a blob opened under another
    /// kind's pair, which a grant for a different kind would carry.
    #[test]
    fn neither_a_backupkey_chunk_nor_another_kinds_pair_opens() {
        let backup = BackupKey::derive(&[7u8; 32]);
        let old_form =
            fauna_core::crypto::seal_backup_chunk(&trained().to_bytes(), &backup, |e| e).unwrap();
        assert!(matches!(
            unseal_topic_model(&old_form, &key()),
            Err(ModelSealError::Decrypt(_))
        ));

        let blob = seal_topic_model(&trained(), &key()).unwrap();
        let other = DelegableSchedule::derive(&backup).for_kind("fauna.state.personalization");
        assert!(matches!(
            unseal_topic_model(&blob, &other),
            Err(ModelSealError::Decrypt(_))
        ));
    }
}
