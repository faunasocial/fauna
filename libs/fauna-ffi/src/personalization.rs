//! UniFFI façade for the **trained-topic lifecycle**
//! (`docs/goal/behavior/topic-factors.md` § Authoring surface & picker).
//!
//! Five thin wrappers, named after the shared gestures they call, over the SHARED
//! `fauna_client_personalization::topics::TrainedTopics` service — the same
//! registry↔model-plane sequencing (the advisory example-count read, the
//! create cap, and the delete's registry-removal-then-`model.delete` pairing)
//! the wasm `{list,create,rename,delete}TrainedTopic(s)` faces bind
//! (`libs/fauna-wasm/src/rpc.rs`). Windows is the first FFI (native-client)
//! consumer — these are wrappers, not a re-derivation of the pairing
//! (priority #2). Each wrapper goes through the shared
//! `fauna_sync_engine::preference_surfaces::*_trained_topic*` gestures (the
//! same calls tui and linux make): the registry rides the account store of
//! this process's runtime (`crate::account_runtime::handle_source()`, waited
//! for when a call arrives before the assembly has landed), while the
//! model-plane leg stays a nest call (`config-dissolution.md` § The `__config`
//! dissolution schedule → *The closure order*, steps (1) and (5); the registry
//! rests on `fauna.state.personalization`). The
//! per-post train gestures themselves
//! (`train_post`/`untrain_post`/`train_target_factor`/`example_label_for`/
//! `is_muted`) are already exported on [`crate::feed_manager::FfiFeedManager`].
//!
//! Ids cross the boundary as raw 16-byte arrays (`Vec<u8>`), matching
//! `FfiPeerContact.actor_id` (`src/peer.rs`) rather than the wasm hex-string
//! convention — native apps pass byte arrays natively; the picker's
//! FlaUI-visible stable key still rides as hex text (`AutomationProperties.Name`),
//! same shape as web's `data-factor` attribute.

use std::sync::Arc;

use fauna_client_personalization::publish::{
    PublishListError, PublishModelError, publish_trained_factor_list, publish_trained_factor_model,
};
use fauna_client_personalization::{TrainedTopicRow, TrainedTopics, TrainedTopicsError};
use fauna_core::scoring::{ListArtifactError, MAX_LABELER_LIST_NAME_LEN, TextModelArtifactError};
use fauna_sync_engine::preference_surfaces;

use crate::nest_client::FfiNestClient;

/// The rejection shape of a trained-topic gesture. Structured, not a
/// pre-formatted sentence: `TrainedTopicsError::Display` is hard-coded
/// English, and the cap/blank-name cases are user-facing product messages
/// every app already localizes (`personalization.trained_factor_cap`) — so
/// the boundary carries a machine-readable variant (+ the cap's `max`) rather
/// than the Rust text (mirrors `topics_err_to_js` in `libs/fauna-wasm/src/rpc.rs`).
#[derive(uniffi::Error, Debug, thiserror::Error, PartialEq, Eq)]
pub enum FfiTrainedTopicsError {
    /// The registry is full (`TRAINED_FACTORS_MAX`) — the client-side twin of
    /// the nest's create-cap, hit at *create* rather than first train.
    #[error("at most {max} trained topics")]
    Cap { max: u32 },
    /// The name was empty (or all whitespace) — distinct from `Cap` so a
    /// client never tells a user who left the box empty that they ran out of
    /// topics.
    #[error("a trained topic needs a name")]
    BlankName,
    /// The account-store read/write, or the model-plane call, failed.
    #[error("{msg}")]
    General { msg: String },
}

impl From<TrainedTopicsError> for FfiTrainedTopicsError {
    fn from(e: TrainedTopicsError) -> Self {
        match e {
            TrainedTopicsError::Cap(max) => Self::Cap { max: max as u32 },
            TrainedTopicsError::BlankName => Self::BlankName,
            TrainedTopicsError::Config(msg) | TrainedTopicsError::Model(msg) => {
                Self::General { msg }
            }
        }
    }
}

/// One row of the Trained-topics facet — the registry entry plus its advisory
/// example count off the model plane (mirrors `fauna_client_personalization::TrainedTopicRow`).
#[derive(uniffi::Record)]
pub struct FfiTrainedTopicRow {
    /// The factor's 16-byte registry id (stable across rename; the picker's
    /// `AutomationProperties.Name` carries its hex form).
    pub id: Vec<u8>,
    /// The user's chosen display name. Sealed — the nest never sees it.
    pub name: String,
    /// The composition key (`topic:<hex>`) the picker offers and the train
    /// gestures target. `None` only for a corrupt (non-16-byte) id.
    pub factor_key: Option<String>,
    /// Advisory count of explicit examples trained into this factor, as
    /// stored nest-side. `0` for a factor never trained.
    pub example_count: u32,
    /// The per-factor Layer-A opt-in ("Learn from my activity"): when on, the
    /// feed's engagement cues weak-train this factor. Off per v1 until the
    /// user flips the row's toggle.
    pub learn_from_engagement: bool,
}

impl From<TrainedTopicRow> for FfiTrainedTopicRow {
    fn from(r: TrainedTopicRow) -> Self {
        Self {
            id: r.id,
            name: r.name,
            factor_key: r.factor_key,
            example_count: r.example_count,
            learn_from_engagement: r.learn_from_engagement,
        }
    }
}

fn service(nest: &Arc<FfiNestClient>) -> TrainedTopics<Arc<fauna_client::NestClient>> {
    TrainedTopics::new(nest.nest_arc())
}

fn rows_to_ffi(rows: Vec<TrainedTopicRow>) -> Vec<FfiTrainedTopicRow> {
    rows.into_iter().map(FfiTrainedTopicRow::from).collect()
}

/// The owner's trained topics — one row per registry entry, each carrying the
/// advisory example count off the model plane.
#[fauna_uniffi_async::export]
pub async fn list_trained_topics(
    nest: Arc<FfiNestClient>,
) -> Result<Vec<FfiTrainedTopicRow>, FfiTrainedTopicsError> {
    let t = service(&nest);
    let store = crate::account_runtime::handle_source();
    Ok(rows_to_ffi(
        preference_surfaces::list_trained_topics(&store, &t).await?,
    ))
}

/// Mint a trained topic. Rejects a blank name, and the registry cap
/// (`TRAINED_FACTORS_MAX`). Returns the fresh row list.
#[fauna_uniffi_async::export]
pub async fn create_trained_topic(
    nest: Arc<FfiNestClient>,
    name: String,
) -> Result<Vec<FfiTrainedTopicRow>, FfiTrainedTopicsError> {
    let t = service(&nest);
    let store = crate::account_runtime::handle_source();
    Ok(rows_to_ffi(
        preference_surfaces::create_trained_topic(&store, &t, &name).await?,
    ))
}

/// Rename a trained topic in place. The id — and so the derived composition
/// key — is untouched, so every feed composing this factor keeps working.
#[fauna_uniffi_async::export]
pub async fn rename_trained_topic(
    nest: Arc<FfiNestClient>,
    id: Vec<u8>,
    name: String,
) -> Result<Vec<FfiTrainedTopicRow>, FfiTrainedTopicsError> {
    let t = service(&nest);
    let store = crate::account_runtime::handle_source();
    Ok(rows_to_ffi(
        preference_surfaces::rename_trained_topic(&store, &t, &id, &name).await?,
    ))
}

/// Flip a trained topic's Layer-A opt-in (`learn_from_engagement` — the row's
/// "Learn from my activity" toggle). Registry-only: the model row is
/// untouched, so turning it off stops future weak training without rewriting
/// what engagement already taught. Unknown id is a no-op; the returned rows
/// show the current truth.
#[fauna_uniffi_async::export]
pub async fn set_trained_topic_engagement(
    nest: Arc<FfiNestClient>,
    id: Vec<u8>,
    on: bool,
) -> Result<Vec<FfiTrainedTopicRow>, FfiTrainedTopicsError> {
    let t = service(&nest);
    let store = crate::account_runtime::handle_source();
    Ok(rows_to_ffi(
        preference_surfaces::set_trained_topic_engagement(&store, &t, &id, on).await?,
    ))
}

/// Delete a trained topic: the registry entry AND its paired nest-side model
/// row. Compositions still naming the key stay valid (the zero-term seam
/// makes an orphan key inert).
#[fauna_uniffi_async::export]
pub async fn delete_trained_topic(
    nest: Arc<FfiNestClient>,
    id: Vec<u8>,
) -> Result<Vec<FfiTrainedTopicRow>, FfiTrainedTopicsError> {
    let t = service(&nest);
    let store = crate::account_runtime::handle_source();
    Ok(rows_to_ffi(
        preference_surfaces::delete_trained_topic(&store, &t, &id).await?,
    ))
}

// ── Publishing a trained factor as a List (topic-factors.md § Publishing) ──

/// The rejection shape of a publish. Same reasoning as
/// [`FfiTrainedTopicsError`]: a machine-readable variant for each case a user
/// can actually cause and a client must localize, plus the bound the crate
/// knows and the client does not. Everything else is a shell bug or a dead
/// nest, and collapses to `General`.
#[derive(uniffi::Error, Debug, thiserror::Error, PartialEq, Eq)]
pub enum FfiPublishListError {
    /// The publisher-chosen public name was empty (or all whitespace).
    #[error("a published list needs a name")]
    BlankName,
    /// The name exceeded `MAX_LABELER_LIST_NAME_LEN` — a published name is
    /// public, so unlike the sealed registry name it is bounded.
    #[error("a published name is at most {max} characters")]
    NameTooLong { max: u32 },
    /// A malformed entry, an unsignable metadata blob, or a nest that refused
    /// or could not be reached. Each already names its own cause.
    #[error("{msg}")]
    General { msg: String },
}

impl From<PublishListError> for FfiPublishListError {
    fn from(e: PublishListError) -> Self {
        match e {
            PublishListError::Artifact(ListArtifactError::BlankName) => Self::BlankName,
            PublishListError::Artifact(ListArtifactError::NameTooLong(_)) => Self::NameTooLong {
                max: MAX_LABELER_LIST_NAME_LEN as u32,
            },
            other => Self::General {
                msg: other.to_string(),
            },
        }
    }
}

/// One exemplar the user kept in the review sheet.
///
/// Deliberately **not** [`fauna_feed::ScoredExemplar`], though the sheet's rows
/// are exactly that: an exemplar carries the post's `preview` text, and this
/// type is the boundary that makes it unmissable that the preview does **not**
/// cross — a published List is `content_id → score` and nothing else. For a
/// feature whose whole promise is "subscribers see exactly the ids you chose",
/// the type is the cheapest place to say so.
#[derive(uniffi::Record)]
pub struct FfiPublishEntry {
    /// Hex-encoded 32-byte content id (`ScoredExemplar.post_id`, verbatim).
    pub post_id: String,
    /// The factor's per-mille score ∈ [0,1000], published with no rescale.
    pub score: i64,
}

/// What landed on the registry (mirrors
/// `fauna_client_personalization::publish::PublishedList`).
#[derive(uniffi::Record)]
pub struct FfiPublishedList {
    /// The published artifact's id — the **derived** verifying key, not the
    /// publishing actor (§ Publishing: publishing is pseudonymous).
    pub labeler_id: Vec<u8>,
    /// The version now live: `1` for a first publish, else one past the
    /// catalog's.
    pub version: u64,
    /// Entries actually published — post-dedup, so a shell can honestly report
    /// what landed rather than what it sent.
    pub entry_count: u32,
}

/// Publish a trained factor's kept exemplars as a tier-3 List labeler
/// (`topic-factors.md` § Publishing a trained factor; the frame's D8).
///
/// A thin wrapper over the shared
/// `fauna_client_personalization::publish::publish_trained_factor_list`, which
/// owns the entire lifecycle — deriving the per-factor keypair, resolving the
/// next version off the catalog, building + signing the artifact, and the
/// `fauna.labelers.publish` call. No client re-derives any of it.
///
/// `entries` is the **pruned** set the review sheet handed over: whatever the
/// user left checked, in whatever order the sheet rendered (the shared builder
/// owns the sort/dedup/validate into the canonical form the nest gate demands).
/// `name` is the publisher-chosen **public** display name — the sealed registry
/// name stays private, so a shell must never default this to it. The signed
/// `updated_at` is stamped by the shared lifecycle (no shell supplies a clock,
/// or its unit).
#[fauna_uniffi_async::export]
pub async fn trained_topic_publish_list(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
    factor_id: Vec<u8>,
    name: String,
    entries: Vec<FfiPublishEntry>,
) -> Result<FfiPublishedList, FfiPublishListError> {
    let secret = crate::crypto::secret32(&owner_secret)
        .map_err(|e| FfiPublishListError::General { msg: e.to_string() })?;
    let id16: [u8; 16] =
        factor_id
            .as_slice()
            .try_into()
            .map_err(|_| FfiPublishListError::General {
                msg: "factor_id must be 16 bytes".into(),
            })?;
    let published = publish_trained_factor_list(
        nest.nest_arc(),
        &secret,
        &id16,
        &name,
        entries.into_iter().map(|e| (e.post_id, e.score)).collect(),
    )
    .await?;
    Ok(FfiPublishedList {
        labeler_id: published.labeler_id.to_vec(),
        version: published.version,
        entry_count: published.entry_count as u32,
    })
}

/// Why a **model** publish did not happen — the [`FfiPublishListError`] shape
/// with the one variant a vocabulary has and a list does not.
///
/// Same rule as the List's: only what a *user* can cause and a client must
/// localize gets a variant, plus the bound the crate knows and the client does
/// not. Everything else is a shell bug or a dead nest, and collapses to
/// `General`.
#[derive(uniffi::Error, Debug, thiserror::Error, PartialEq, Eq)]
pub enum FfiPublishModelError {
    /// Nothing cleared the shared distinct-document prune floor, so there is no
    /// vocabulary to publish — in practice the factor has too few *public*
    /// examples. A distinct variant because it is the one refusal a user fixes
    /// by marking more posts rather than by editing the sheet, and § Publishing
    /// mandates copy that says exactly that.
    #[error("this topic needs more public examples before it can be shared as a model")]
    EmptyVocabulary,
    /// The publisher-chosen public name was empty (or all whitespace).
    #[error("a published model needs a name")]
    BlankName,
    /// The name exceeded `MAX_LABELER_LIST_NAME_LEN` — a published name is
    /// public, so unlike the sealed registry name it is bounded.
    #[error("a published name is at most {max} characters")]
    NameTooLong { max: u32 },
    /// A malformed vocabulary, an unsignable metadata blob, or a nest that
    /// refused or could not be reached. Each already names its own cause.
    #[error("{msg}")]
    General { msg: String },
}

impl From<PublishModelError> for FfiPublishModelError {
    fn from(e: PublishModelError) -> Self {
        match e {
            PublishModelError::EmptyVocabulary => Self::EmptyVocabulary,
            PublishModelError::Artifact(TextModelArtifactError::BlankName) => Self::BlankName,
            PublishModelError::Artifact(TextModelArtifactError::NameTooLong(_)) => {
                Self::NameTooLong {
                    max: MAX_LABELER_LIST_NAME_LEN as u32,
                }
            }
            other => Self::General {
                msg: other.to_string(),
            },
        }
    }
}

/// One n-gram the user kept in the Model review sheet.
///
/// Deliberately **not** [`fauna_feed::ReviewNgram`], though the sheet's rows are
/// exactly that — the same reason [`FfiPublishEntry`] is not `ScoredExemplar`:
/// the review type is what a shell *renders*, this is what it *publishes*, and
/// keeping them apart is what makes it unmissable that the two need not be the
/// same set. That they carry identical fields today is a fact about this
/// vocabulary, not a licence to publish whatever was rendered.
#[derive(uniffi::Record)]
pub struct FfiPublishNgram {
    /// The n-gram itself, verbatim off the review row.
    pub ngram: String,
    /// Distinct *more like this* example documents it occurred in.
    pub more: u32,
    /// Distinct *less like this* example documents it occurred in.
    pub less: u32,
}

/// What landed on the registry (mirrors
/// `fauna_client_personalization::publish::PublishedModel`).
#[derive(uniffi::Record)]
pub struct FfiPublishedModel {
    /// The published artifact's id — the **derived** verifying key, not the
    /// publishing actor (§ Publishing: publishing is pseudonymous). The same id
    /// a List publish of this factor would produce: the kind is per-version
    /// under one publisher identity, so a subscriber keeps their subscription
    /// across a List -> Model upgrade.
    pub labeler_id: Vec<u8>,
    /// The version now live: `1` for a first publish, else one past the
    /// catalog's — for this factor, whatever kind that version was.
    pub version: u64,
    /// N-grams actually published — post-dedup, read back out of the signed
    /// bytes, so a shell reports what landed rather than what it sent.
    pub ngram_count: u32,
    /// Documents the vocabulary was built from (`more_docs + less_docs`) — the
    /// N of the sheet's "built from N public examples" line.
    pub document_count: u32,
}

/// Publish a trained factor's **scrubbed vocabulary** as a tier-3 `text-model`
/// labeler (`topic-factors.md` § Publishing a trained factor, v2).
///
/// A thin wrapper over the shared
/// `fauna_client_personalization::publish::publish_trained_factor_model`, which
/// owns the entire lifecycle — deriving the per-factor keypair, resolving the
/// next version off the catalog, building + signing the artifact, and the
/// `fauna.labelers.publish` call. No client re-derives any of it, and the
/// identity half is the List's unchanged, which is what makes upgrading a
/// factor from List to Model an ordinary version bump.
///
/// `ngrams` is the **pruned** set the review sheet left checked, in whatever
/// order it rendered (the shared builder owns the sort/dedup/validate into the
/// canonical form the nest gate demands). `more_docs`/`less_docs` are the
/// **corpus** counters from `FfiFeedManager::scrub_corpus_for_factor` and are
/// passed through **unshrunk**: they say how many public examples the
/// vocabulary was built from, which stays true however much of it the user
/// withheld. A shell that "corrects" them down to match the pruned rows would
/// silently make the published model look more confident than it is.
///
/// `name` is the publisher-chosen **public** display name — the sealed registry
/// name stays private, so a shell must never default this to it. The signed
/// `updated_at` is stamped by the shared lifecycle (no shell supplies a clock,
/// or its unit).
///
/// The private model is not a parameter, and must never become one: the counts
/// signed here come from a rebuild over public post text.
#[fauna_uniffi_async::export]
pub async fn trained_topic_publish_model(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
    factor_id: Vec<u8>,
    name: String,
    more_docs: u32,
    less_docs: u32,
    ngrams: Vec<FfiPublishNgram>,
) -> Result<FfiPublishedModel, FfiPublishModelError> {
    let secret = crate::crypto::secret32(&owner_secret)
        .map_err(|e| FfiPublishModelError::General { msg: e.to_string() })?;
    let id16: [u8; 16] =
        factor_id
            .as_slice()
            .try_into()
            .map_err(|_| FfiPublishModelError::General {
                msg: "factor_id must be 16 bytes".into(),
            })?;
    let published = publish_trained_factor_model(
        nest.nest_arc(),
        &secret,
        &id16,
        &name,
        more_docs,
        less_docs,
        ngrams
            .into_iter()
            .map(|n| (n.ngram, n.more, n.less))
            .collect(),
    )
    .await?;
    Ok(FfiPublishedModel {
        labeler_id: published.labeler_id.to_vec(),
        version: published.version,
        ngram_count: published.ngram_count as u32,
        document_count: published.document_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_error_carries_the_limit() {
        let e: FfiTrainedTopicsError = TrainedTopicsError::Cap(32).into();
        assert_eq!(e, FfiTrainedTopicsError::Cap { max: 32 });
    }

    #[test]
    fn publish_blank_name_is_its_own_variant_not_general() {
        // The one publish case a user causes by leaving a box empty; it must
        // never reach them as the crate's internal "name is present but blank".
        let e: FfiPublishListError =
            PublishListError::Artifact(ListArtifactError::BlankName).into();
        assert_eq!(e, FfiPublishListError::BlankName);
    }

    #[test]
    fn publish_name_too_long_carries_the_bound() {
        // The client localizes the message and needs the number to do it —
        // the same reason `Cap` carries `max`.
        let e: FfiPublishListError =
            PublishListError::Artifact(ListArtifactError::NameTooLong(9_999)).into();
        assert_eq!(
            e,
            FfiPublishListError::NameTooLong {
                max: MAX_LABELER_LIST_NAME_LEN as u32
            }
        );
    }

    #[test]
    fn model_publish_empty_vocabulary_is_its_own_variant() {
        // The one model refusal a user fixes by marking MORE POSTS rather than
        // by editing the sheet. Collapsing it into `General` would hand them the
        // crate's internal sentence instead of the copy Publishing mandates.
        let e: FfiPublishModelError = PublishModelError::EmptyVocabulary.into();
        assert_eq!(e, FfiPublishModelError::EmptyVocabulary);
    }

    #[test]
    fn model_publish_name_errors_match_the_list_shape() {
        // A published name is a published name whatever the artifact kind, so
        // the two boundaries must not disagree about which name failures are
        // localizable — the same two variants, carrying the same bound.
        let blank: FfiPublishModelError =
            PublishModelError::Artifact(TextModelArtifactError::BlankName).into();
        assert_eq!(blank, FfiPublishModelError::BlankName);

        let long: FfiPublishModelError =
            PublishModelError::Artifact(TextModelArtifactError::NameTooLong(9_999)).into();
        assert_eq!(
            long,
            FfiPublishModelError::NameTooLong {
                max: MAX_LABELER_LIST_NAME_LEN as u32
            }
        );
    }

    #[test]
    fn model_publish_vocabulary_faults_collapse_to_general() {
        // A shell cannot act on "entry 3 is below the prune floor" — the scrub
        // produced the rows, so a fault in them is a bug on this side of the
        // boundary, not a user error to localize.
        let e: FfiPublishModelError =
            PublishModelError::Artifact(TextModelArtifactError::BelowPruneFloor {
                index: 3,
                docs: 1,
            })
            .into();
        assert!(matches!(e, FfiPublishModelError::General { .. }), "{e:?}");
    }

    #[test]
    fn publish_transport_collapses_to_general() {
        let e: FfiPublishListError = PublishListError::Transport("nest down".into()).into();
        assert_eq!(
            e,
            FfiPublishListError::General {
                msg: "nest: nest down".into()
            }
        );
    }

    #[test]
    fn blank_name_is_its_own_variant_not_general() {
        let e: FfiTrainedTopicsError = TrainedTopicsError::BlankName.into();
        assert_eq!(e, FfiTrainedTopicsError::BlankName);
    }
}
