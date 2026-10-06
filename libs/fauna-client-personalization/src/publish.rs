//! Publishing a trained factor as a **List** labeler — the voluntary act that
//! turns a private tier-1 model into a subscribable tier-3 artifact.
//!
//! `topic-factors.md` § Publishing a trained factor (frame D8).
//!
//! v1 publishes what the factor *found*, never what it *learned*: the client
//! scores the user's known public-post corpus, the user prunes the exemplars
//! they would rather not endorse, and the surviving `content_id → score` map
//! ships as a `list` artifact over the existing `fauna.labelers.publish`. No
//! model bytes cross — subscribers see exactly the ids the publisher chose to
//! show. (Publishing the model itself is v2, gated on its own privacy-scrub
//! design pass.)
//!
//! # The publishing identity is pseudonymous, and derived per factor
//!
//! Labelers are **public-key-is-identity**: the nest sets `labeler_id =
//! algorithm_id = publisher_actor` from the signing key alone
//! (`labeler_handlers.rs` `publish_labeler_core`). Signing with the user's own
//! actor key would therefore fuse two things that must stay apart:
//!
//! * **One key, one labeler.** `labeler_id` *is* the key, and the nest enforces
//!   a monotonic version per `labeler_id` — so an actor-key-signed user could
//!   publish exactly **one** list, forever, and their second topic would collide
//!   with their first as a "version bump" of an unrelated artifact.
//! * **Identity conflation.** Every subscriber would learn that this labeler is
//!   this actor, and every published list would be provably the same person's —
//!   in a feature whose whole premise (§ Publishing) is that the artifact is a
//!   *derivative of private behavior* shared by explicit, reviewed choice.
//!
//! So the signing keypair is **derived per factor** from the actor secret and
//! the factor's registry id ([`labeler_signing_seed`]). That buys three
//! properties at once, with no new persisted state: many lists per user, each
//! unlinkable to the actor and to each other; a stable id per factor, so a
//! **republish re-derives the same key** and the nest's monotonic version bump
//! lands on the right artifact; and nothing to lose — the key is a pure function
//! of state the client already holds, so it survives a device wipe like the
//! [`crate::ModelSealKey`] does.
//!
//! The derivation is the house [`fauna_core::crypto::BackupKey::derive`] shape:
//! BLAKE3 `derive_key` under a dated, domain-separated context, so the signing
//! key is cryptographically independent of the backup key and of the actor's own
//! Ed25519 key even though all three descend from the same secret.

use ed25519_dalek::SigningKey;
use fauna_client_labelers::LabelersClient;
use fauna_core::data::Timestamp;
use fauna_core::encoding::{canonical_encode, content_hash};
use fauna_core::hex32::{self, Hex32Error};
use fauna_core::identity::ActorId;
use fauna_core::scoring::{
    AlgorithmLabeler, LabelerInput, LabelerOutput, ListArtifactError, ScorerLimits,
    TextModelArtifactError, artifact_kind, build_list_artifact, build_text_model_artifact,
    sign_labeler_metadata, validate_list_artifact, validate_text_model_artifact,
};
use fauna_protocol::RpcRequester;

/// Domain-separation context for the per-factor labeler signing key. Dated and
/// purpose-named in the [`fauna_core::crypto::BackupKey::derive`] convention
/// ("fauna backup encryption key 2026-03-12"): the date pins *this* derivation,
/// so a future scheme change mints a new context rather than silently
/// reinterpreting keys already published under this one.
const LABELER_PUBLISH_CONTEXT: &str = "fauna labeler publish signing key 2026-07-16";

/// How many scored exemplars the review-prune sheet offers — **every app's**
/// sheet, so the bound is product behavior, not a shell choice.
///
/// Bounded by what a human will actually read before endorsing it, not by the
/// artifact cap (`MAX_LABELER_LIST_ENTRIES` = 16_384, three orders of magnitude
/// larger): every entry published is a post the user is vouching for in public,
/// so an unreviewably long list would make the prune a rubber stamp — the one
/// thing § Publishing does not allow the sheet to become. The FFI/wasm corpus
/// faces apply it themselves (a shell never picks its own N); native callers of
/// `FeedManager::score_corpus_for_factor` pass it explicitly.
pub const REVIEW_TOP_N: usize = 50;

/// Derive the publishing keypair seed for one trained factor — the pseudonymous
/// per-factor identity described in the module docs.
///
/// `blake3::derive_key(LABELER_PUBLISH_CONTEXT, actor_secret || factor_id)`,
/// streamed rather than concatenated into a buffer: the two forms are
/// byte-identical (BLAKE3's `derive_key` hashes its key material as a stream —
/// pinned by `the_signing_seed_is_the_domain_separated_concatenation`), and
/// streaming avoids materializing a second copy of the actor secret on the
/// stack just to hash it.
///
/// Deterministic by construction: same actor + same factor ⇒ same key ⇒ same
/// `labeler_id`, which is exactly what makes republish a *version bump* of the
/// existing list rather than a new, orphaned artifact.
pub fn labeler_signing_seed(actor_secret: &[u8; 32], factor_id: &[u8; 16]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key(LABELER_PUBLISH_CONTEXT);
    hasher.update(actor_secret);
    hasher.update(factor_id);
    *hasher.finalize().as_bytes()
}

/// What landed on the nest's registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedList {
    /// The published artifact's id — the derived verifying key. The nest sets
    /// `labeler_id = algorithm_id` (and `publisher_actor` to the same), so this
    /// is the id a subscriber inspects and subscribes to.
    pub labeler_id: [u8; 32],
    /// The version now live. `1` for a first publish, else one past whatever the
    /// catalog reported.
    pub version: u64,
    /// Entries actually published — **post-dedup**, so a UI can honestly report
    /// "published 40 posts" when the caller handed over 42 with two repeats.
    pub entry_count: usize,
}

/// Why a publish did not happen. Typed rather than pre-formatted, the
/// [`crate::TrainedTopicsError`] convention: the entry errors carry the offending
/// row's `index` so a client can point at it, and each shell localizes.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PublishListError {
    /// No entries survived the prune. An empty List *means* nothing — there is
    /// nothing for a subscriber to inspect or compose — so publishing one is
    /// always a caller bug, refused **here** rather than left to each app's
    /// widget logic: the artifact layer itself accepts zero entries (an empty
    /// map is canonically well-formed), so without this guard every shell must
    /// independently remember to disarm its submit, and the one that forgets
    /// silently publishes a meaningless artifact under the user's name.
    #[error("a published list needs at least one entry")]
    NoEntries,
    /// An entry's `post_id` is not valid hex. Never silently skipped: the caller
    /// asked for these exact posts, and quietly publishing a shorter list than
    /// the user reviewed would break the one promise this feature makes —
    /// subscribers see exactly what the publisher chose to show.
    #[error("entry {index} post_id is not valid hex: {reason}")]
    ContentIdHex { index: usize, reason: String },
    /// An entry's `post_id` decoded, but not to a 32-byte content id.
    #[error("entry {index} post_id is {len} bytes (expected 32)")]
    ContentIdLen { index: usize, len: usize },
    /// The artifact failed to build or validate — including a blank `name`, an
    /// over-long one, an out-of-range score, or an over-cap entry count
    /// ([`ListArtifactError`]). Raised **before** the wire: the publisher must
    /// never learn at the nest gate that the bytes it signed were out of
    /// contract.
    #[error("list artifact: {0}")]
    Artifact(#[from] ListArtifactError),
    /// The metadata could not be signed or canonically encoded. Unreachable for
    /// a well-formed key over the frozen [`AlgorithmLabeler`] shape — carried as
    /// a variant only because `sign_labeler_metadata` / `canonical_encode` are
    /// fallible, never as a case a shell is expected to render specially.
    #[error("labeler metadata: {0}")]
    Metadata(String),
    /// The nest rejected the call, or could not be reached.
    #[error("nest: {0}")]
    Transport(String),
}

/// Publish a trained factor's scored exemplars as a List labeler.
///
/// `entries` are `(hex-encoded 32-byte post_id, per-mille score)` — the pruned
/// set the review sheet handed over, in whatever order it rendered.
/// [`build_list_artifact`] owns the sort/dedup/validate into the canonical form
/// the nest gate demands, so no client re-derives it.
///
/// `name` is the **publisher-chosen public** display name; the sealed registry
/// name stays private (§ Publishing). It rides inside the artifact, not on the
/// wire — the signed metadata shape is frozen.
///
/// The metadata's `updated_at` is stamped **here** ([`Timestamp::now`], the
/// house microseconds-since-epoch): it is "when this publish happened", not a
/// caller choice — and a per-app parameter is exactly how the first caller
/// came to sign *seconds* into an artifact whose type reads microseconds.
pub async fn publish_trained_factor_list<R: RpcRequester>(
    nest: R,
    actor_secret: &[u8; 32],
    factor_id: &[u8; 16],
    name: &str,
    entries: Vec<(String, i64)>,
) -> Result<PublishedList, PublishListError> {
    if entries.is_empty() {
        return Err(PublishListError::NoEntries);
    }
    let signing_key = SigningKey::from_bytes(&labeler_signing_seed(actor_secret, factor_id));
    // Public-key-is-identity: `algorithm_id` IS the verifying key, and the nest
    // derives `labeler_id` / `publisher_actor` from it rather than from the
    // authenticated connection — which is what keeps this publish pseudonymous.
    let algorithm_id = ActorId(signing_key.verifying_key().to_bytes());

    let mut decoded = Vec::with_capacity(entries.len());
    for (index, (post_id, score)) in entries.into_iter().enumerate() {
        let content_id: [u8; 32] = hex32::decode(&post_id).map_err(|e| match e {
            Hex32Error::NotHex(reason) => PublishListError::ContentIdHex { index, reason },
            Hex32Error::WrongLength(len) => PublishListError::ContentIdLen { index, len },
        })?;
        decoded.push((content_id, score));
    }

    let artifact_bytes = build_list_artifact(Some(name), decoded)?;
    // Read the count back out of the bytes we are about to sign, rather than
    // counting the input: the builder dedups, so the input count can overstate.
    // `validate_list_artifact` is the *same* function, over the *same* bytes,
    // that the nest gate runs to decide how many `content_scores` rows to
    // materialize — so this count cannot drift from what actually lands.
    let entry_count = validate_list_artifact(&artifact_bytes)?.entries.len();

    let version = publish_factor_artifact(
        nest,
        &signing_key,
        algorithm_id,
        artifact_bytes,
        artifact_kind::LIST,
    )
    .await?;

    // Our own derived id and the version we signed, not the reply's echo: the
    // nest sets `labeler_id = algorithm_id` and stores the version it accepted
    // from the metadata, so a reply that disagreed would be a nest bug, not a
    // case to model here.
    Ok(PublishedList {
        labeler_id: algorithm_id.0,
        version,
        entry_count,
    })
}

/// What landed on the registry for a published **model**.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedModel {
    /// The published artifact's id — the derived verifying key. **The same id a
    /// List publish of this factor would produce**: the kind is per-version
    /// under one publisher identity, so a subscriber keeps their subscription
    /// across a List → Model upgrade.
    pub labeler_id: [u8; 32],
    /// The version now live. `1` for a first publish, else one past whatever the
    /// catalog reported — for **this factor**, whatever kind that version was.
    pub version: u64,
    /// N-grams actually published — post-dedup, read back out of the signed
    /// bytes (the `PublishedList::entry_count` convention).
    pub ngram_count: usize,
    /// Documents the vocabulary was built from (`more_docs + less_docs`) — what
    /// the sheet's "built from N public examples" line reports.
    pub document_count: u32,
}

/// Why a model publish did not happen. The [`PublishListError`] shape, minus the
/// content-id variants a vocabulary has no analogue for.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PublishModelError {
    /// The scrubbed vocabulary is empty — no n-gram cleared the distinct-document
    /// prune floor, which in practice means the factor has fewer than
    /// `TEXT_MODEL_PUBLISH_MIN_DOCS` public examples. Refused **here**, exactly
    /// as the List's empty-set refusal is: the artifact layer accepts an empty
    /// vocabulary (it is canonically well-formed), so without this guard every
    /// shell must independently remember to disarm its submit, and the one that
    /// forgets publishes a model that scores every post neutral under the user's
    /// name. § Publishing mandates copy telling the user the factor needs more
    /// public examples.
    #[error("a published model needs at least one n-gram above the prune floor")]
    EmptyVocabulary,
    /// The artifact failed to build or validate — a blank or over-long `name`, an
    /// over-cap vocabulary, a count above its class document counter, or an entry
    /// below the prune floor ([`TextModelArtifactError`]). Raised **before** the
    /// wire: the publisher must never learn at the nest gate that the bytes it
    /// signed were out of contract.
    #[error("text-model artifact: {0}")]
    Artifact(#[from] TextModelArtifactError),
    /// The metadata could not be signed or canonically encoded.
    #[error("labeler metadata: {0}")]
    Metadata(String),
    /// The nest rejected the call, or could not be reached.
    #[error("nest: {0}")]
    Transport(String),
}

impl From<PublishArtifactError> for PublishModelError {
    fn from(e: PublishArtifactError) -> Self {
        match e {
            PublishArtifactError::Metadata(m) => PublishModelError::Metadata(m),
            PublishArtifactError::Transport(t) => PublishModelError::Transport(t),
        }
    }
}

/// Publish a trained factor's **scrubbed vocabulary** as a `text-model` labeler
/// (`topic-factors.md` § Publishing a trained factor, v2).
///
/// `ngrams` is the `(ngram, more, less)` set the review sheet left checked, in
/// whatever order it rendered — [`build_text_model_artifact`] owns the
/// sort/dedup/validate into canonical form, so no client re-derives it.
/// `more_docs`/`less_docs` are the **corpus** counters from the scrub and do
/// **not** shrink when the user prunes an n-gram: they describe how many public
/// examples the vocabulary was built from, which stays true however much of that
/// vocabulary the user chose to withhold. (They are also the posterior's priors
/// and the cold-start damp's sample count, so lowering them to match a pruned
/// vocabulary would silently make the model look more confident than it is.)
///
/// Everything about the publishing identity is the List's, unchanged: the same
/// per-factor derived keypair, the same catalog-resolved monotonic version, the
/// same `content_kind: "post"`. That is what makes upgrading a factor's
/// published form from List to Model an ordinary version bump rather than a new,
/// orphaned artifact — and the nest's own consequence of that upgrade
/// (withdrawing the factor's materialized List rows) is the frame's clause, not
/// this function's.
///
/// ⚠ The **private model is not a parameter here, and must never become one.**
/// The counts this function signs come from
/// `fauna_text_model::publish::scrub_corpus` over public post text; threading a
/// `TopicModel` in "for convenience" would reintroduce every leak class the
/// rebuild design exists to kill.
pub async fn publish_trained_factor_model<R: RpcRequester>(
    nest: R,
    actor_secret: &[u8; 32],
    factor_id: &[u8; 16],
    name: &str,
    more_docs: u32,
    less_docs: u32,
    ngrams: Vec<(String, u32, u32)>,
) -> Result<PublishedModel, PublishModelError> {
    if ngrams.is_empty() {
        return Err(PublishModelError::EmptyVocabulary);
    }
    let signing_key = SigningKey::from_bytes(&labeler_signing_seed(actor_secret, factor_id));
    let algorithm_id = ActorId(signing_key.verifying_key().to_bytes());

    let artifact_bytes = build_text_model_artifact(Some(name), more_docs, less_docs, ngrams)?;
    // Read the count back out of the bytes we are about to sign (the List's
    // rule): the builder dedups, so the input count can overstate what a
    // subscriber will actually see.
    let ngram_count = validate_text_model_artifact(&artifact_bytes)?.ngrams.len();

    let version = publish_factor_artifact(
        nest,
        &signing_key,
        algorithm_id,
        artifact_bytes,
        artifact_kind::TEXT_MODEL,
    )
    .await?;

    Ok(PublishedModel {
        labeler_id: algorithm_id.0,
        version,
        ngram_count,
        document_count: more_docs.saturating_add(less_docs),
    })
}

/// Why the shared publish tail ([`publish_factor_artifact`]) did not complete.
/// Kind-neutral: each public lifecycle maps it into its own error surface, so a
/// shell keeps localizing exactly one enum.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
enum PublishArtifactError {
    #[error("{0}")]
    Metadata(String),
    #[error("{0}")]
    Transport(String),
}

impl From<PublishArtifactError> for PublishListError {
    fn from(e: PublishArtifactError) -> Self {
        match e {
            PublishArtifactError::Metadata(m) => PublishListError::Metadata(m),
            PublishArtifactError::Transport(t) => PublishListError::Transport(t),
        }
    }
}

/// The tail every trained-factor publish shares, whatever the artifact kind:
/// resolve the next version off the catalog, sign the frozen metadata over the
/// artifact bytes, and call `fauna.labelers.publish`. Returns the version
/// signed.
///
/// One copy on purpose. The **kind is per-version under one publisher
/// identity** (§ Publishing: a factor may upgrade List → Model as an ordinary
/// version bump), so the version resolution, the metadata↔artifact binding, and
/// the pseudonymous signing key are literally the same mechanism for every
/// kind — duplicating them per kind is how two kinds of the same factor would
/// come to disagree about what version comes next.
async fn publish_factor_artifact<R: RpcRequester>(
    nest: R,
    signing_key: &SigningKey,
    algorithm_id: ActorId,
    artifact_bytes: Vec<u8>,
    kind: &str,
) -> Result<u64, PublishArtifactError> {
    let client = LabelersClient::new(nest);

    // The nest enforces a monotonic version per labeler_id, and a republish
    // re-derives the same key (module docs) — so a republish MUST land one past
    // the live version or be rejected. The catalog is the only place that
    // version is known: the artifact is a fork-at-publish snapshot (§
    // Publishing), so nothing local tracks it. Absent ⇒ this factor has never
    // been published ⇒ version 1.
    let catalog = client
        .list()
        .await
        .map_err(|e| PublishArtifactError::Transport(e.to_string()))?;
    let version = catalog
        .labelers
        .iter()
        .find(|s| s.labeler_id.as_ref() == algorithm_id.0.as_slice())
        .map_or(1, |s| s.version + 1);

    let metadata = sign_labeler_metadata(
        signing_key,
        AlgorithmLabeler {
            algorithm_id,
            version,
            // `wasm_*` reads "artifact" for a data kind — the same
            // metadata↔artifact binding the nest and the FFI holder verify for a
            // WASM module.
            wasm_hash: content_hash(&artifact_bytes),
            wasm_size: artifact_bytes.len() as u64,
            // Both fields below are **meaningless for a data artifact and never
            // read**: the nest's publish gate branches on `artifact_kind`, and
            // the non-WASM arms validate the artifact instead of instantiating a
            // sandbox (`labeler_handlers.rs` `validate_labeler_publish`) — only
            // the WASM arm passes `resource_limits` to `LabelerRuntime::new`,
            // and only a WASM module has inputs to declare. They are non-`Option`
            // in the frozen signed struct (a field-add there would make every
            // older nest reject every labeler, since the signature is checked by
            // re-encoding), so they must carry dummy values.
            input_schema: LabelerInput {
                needs_text: false,
                needs_hashtags: false,
                needs_media_metadata: false,
                needs_author: false,
                needs_attachment_bytes: false,
            },
            output_schema: LabelerOutput::default(),
            resource_limits: ScorerLimits {
                max_memory_bytes: 0,
                max_cpu_microseconds: 0,
            },
            updated_at: Timestamp::now(),
            // Stamped by `sign_labeler_metadata` over the canonical metadata
            // with `signature` zeroed.
            signature: Vec::new(),
        },
    )
    .map_err(|e| PublishArtifactError::Metadata(e.to_string()))?;
    let metadata_blob =
        canonical_encode(&metadata).map_err(|e| PublishArtifactError::Metadata(e.to_string()))?;

    // `content_kind` is ALWAYS "post" — never a parameter, for any kind. Mail is
    // inherently per-recipient, so the nest rejects a `mail` List or Model as
    // malformed (frame § Tier-3 artifact kinds, D10; § Publishing repeats the
    // rule for v2).
    client
        .publish(metadata_blob, artifact_bytes, "post", kind)
        .await
        .map_err(|e| PublishArtifactError::Transport(e.to_string()))?;

    Ok(version)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::block_on;
    use fauna_core::scoring::verify_labeler_metadata;
    use fauna_protocol::ByteBuf;
    use fauna_protocol::labelers::{
        LabelerSummary, ListLabelersReply, PublishLabelerReply, PublishLabelerRequest,
    };

    const SECRET: [u8; 32] = [3u8; 32];
    const FACTOR: [u8; 16] = [7u8; 16];

    /// A recording [`RpcRequester`] that answers `fauna.labelers.list` from a
    /// seeded catalog and captures the last call's kind + canonical payload —
    /// which, after a publish run, is the publish itself.
    ///
    /// Deliberately NOT the shared `fauna_client_testkit::RecordingRequester`:
    /// this double answers `labelers.list` **from seeded state** the
    /// version-resolution assertions vary per test, which the shared double's
    /// stateless `fn`-pointer reply table cannot express. It shares the
    /// testkit's `block_on`; only the stateful half is local.
    #[derive(Default)]
    struct CatalogRecordingRequester {
        catalog: Vec<LabelerSummary>,
        last: std::sync::Mutex<Option<(&'static str, Vec<u8>)>>,
    }

    impl CatalogRecordingRequester {
        fn with_catalog(catalog: Vec<LabelerSummary>) -> Self {
            Self {
                catalog,
                last: Default::default(),
            }
        }

        /// The `fauna.labelers.publish` request this run composed.
        fn published(&self) -> PublishLabelerRequest {
            let (kind, payload) = self.last.lock().unwrap().clone().expect("a call recorded");
            assert_eq!(
                kind, "fauna.labelers.publish",
                "publish must be the last call"
            );
            fauna_protocol::decode_strict(&payload).expect("decodes")
        }
    }

    impl RpcRequester for CatalogRecordingRequester {
        type Error = std::convert::Infallible;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let bytes = fauna_protocol::encode_canonical(&payload).expect("encode request");
            *self.last.lock().unwrap() = Some((kind, bytes.to_vec()));
            let reply = match kind {
                "fauna.labelers.list" => fauna_protocol::encode_canonical(&ListLabelersReply {
                    labelers: self.catalog.clone(),
                    extra: Default::default(),
                }),
                "fauna.labelers.publish" => {
                    fauna_protocol::encode_canonical(&PublishLabelerReply {
                        labeler_id: ByteBuf::from(vec![0xAAu8; 32]),
                        version: 1,
                        ok: true,
                        extra: Default::default(),
                    })
                }
                other => panic!("unexpected kind {other}"),
            }
            .expect("encode reply");
            Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
        }
    }

    /// The id a given (secret, factor) publishes under.
    fn derived_id(actor_secret: &[u8; 32], factor_id: &[u8; 16]) -> [u8; 32] {
        SigningKey::from_bytes(&labeler_signing_seed(actor_secret, factor_id))
            .verifying_key()
            .to_bytes()
    }

    fn hex_id(byte: u8) -> String {
        hex::encode([byte; 32])
    }

    /// A catalog row for `labeler_id` at `version` — only the two fields version
    /// resolution reads are meaningful.
    fn summary(labeler_id: [u8; 32], version: u64) -> LabelerSummary {
        LabelerSummary {
            labeler_id: ByteBuf::from(labeler_id.to_vec()),
            version,
            ..Default::default()
        }
    }

    fn publish(
        rec: &std::sync::Arc<CatalogRecordingRequester>,
        entries: Vec<(String, i64)>,
    ) -> Result<PublishedList, PublishListError> {
        block_on(publish_trained_factor_list(
            rec.clone(),
            &SECRET,
            &FACTOR,
            "Small orange cats",
            entries,
        ))
    }

    /// The happy path, end to end: what reaches the wire is a `list` artifact of
    /// `post` content, canonical by the nest gate's own validator, carrying the
    /// caller's entries sorted ascending.
    #[test]
    fn publish_composes_a_canonical_list_artifact_over_post_content() {
        let rec = std::sync::Arc::new(CatalogRecordingRequester::default());
        // Deliberately out of order: the caller hands over whatever the review
        // sheet rendered; the builder owns the canonical form.
        let out = publish(&rec, vec![(hex_id(0x22), 900), (hex_id(0x11), 400)]).unwrap();

        let req = rec.published();
        assert_eq!(
            req.artifact_kind,
            artifact_kind::LIST,
            "a List must not default to the wasm kind — that sends it to the sandbox gate"
        );
        assert_eq!(
            req.content_kind, "post",
            "a List is public-post-only; the nest rejects a `mail` List as malformed"
        );

        // The nest gate's own validator accepts the bytes we signed.
        let artifact = validate_list_artifact(&req.wasm_bytes).expect("the nest gate accepts it");
        let entries: Vec<(Vec<u8>, i64)> = artifact
            .entries
            .iter()
            .map(|e| (e.content_id.to_vec(), e.score))
            .collect();
        assert_eq!(
            entries,
            vec![([0x11u8; 32].to_vec(), 400), ([0x22u8; 32].to_vec(), 900)],
            "entries must reach the wire sorted ascending by content_id"
        );
        assert_eq!(out.entry_count, 2);
        assert_eq!(out.labeler_id, derived_id(&SECRET, &FACTOR));
    }

    /// The point of composing the metadata here: the blob we send is one the
    /// nest's publish gate accepts **by construction**. `verify_labeler_metadata`
    /// is the shared pre-trust check that gate (and the FFI holder) runs.
    #[test]
    fn the_published_metadata_verifies_against_the_published_artifact_bytes() {
        let rec = std::sync::Arc::new(CatalogRecordingRequester::default());
        publish(&rec, vec![(hex_id(0x11), 400)]).unwrap();

        let req = rec.published();
        let metadata: AlgorithmLabeler =
            fauna_core::encoding::canonical_decode(&req.metadata_blob).expect("metadata decodes");
        verify_labeler_metadata(&metadata, &req.wasm_bytes)
            .expect("the blob must pass the nest's publish gate");

        // The binding the gate checks is over the artifact we actually sent.
        assert_eq!(metadata.wasm_size, req.wasm_bytes.len() as u64);
        assert_eq!(
            metadata.algorithm_id.0,
            derived_id(&SECRET, &FACTOR),
            "public-key-is-identity: the signer IS the labeler id"
        );
    }

    /// The publisher-chosen public name rides inside the artifact (the signed
    /// metadata shape is frozen), so it must survive into the bytes.
    #[test]
    fn the_publisher_chosen_name_round_trips_into_the_artifact() {
        let rec = std::sync::Arc::new(CatalogRecordingRequester::default());
        publish(&rec, vec![(hex_id(0x11), 400)]).unwrap();

        let artifact = validate_list_artifact(&rec.published().wasm_bytes).unwrap();
        assert_eq!(artifact.name.as_deref(), Some("Small orange cats"));
    }

    /// A blank name is the artifact layer's typed error, surfaced before the
    /// wire — not an unnamed list.
    #[test]
    fn a_blank_name_is_a_typed_error_and_never_reaches_the_wire() {
        let rec = std::sync::Arc::new(CatalogRecordingRequester::default());
        let err = block_on(publish_trained_factor_list(
            rec.clone(),
            &SECRET,
            &FACTOR,
            "   ",
            vec![(hex_id(0x11), 400)],
        ))
        .unwrap_err();
        assert_eq!(
            err,
            PublishListError::Artifact(ListArtifactError::BlankName)
        );
        assert!(rec.last.lock().unwrap().is_none(), "nothing may reach nest");
    }

    /// An empty prune is a typed refusal at the SHARED layer, not a per-app
    /// widget obligation: the artifact layer accepts zero entries (an empty map
    /// is canonically well-formed), so a shell that forgot to disarm its submit
    /// would otherwise silently publish a meaningless artifact.
    #[test]
    fn an_empty_entry_set_is_refused_before_the_wire() {
        let rec = std::sync::Arc::new(CatalogRecordingRequester::default());
        let err = publish(&rec, Vec::new()).unwrap_err();
        assert_eq!(err, PublishListError::NoEntries);
        assert!(rec.last.lock().unwrap().is_none(), "nothing may reach nest");
    }

    /// The signed `updated_at` is the house microseconds-since-epoch, stamped
    /// by the lifecycle itself — the first client leg signed *seconds* into
    /// published artifacts, which reads as 1970 to anything rendering the
    /// [`Timestamp`] convention. Bounds, not an exact value: now() is not
    /// injectable, so the pin is the unit's magnitude.
    #[test]
    fn the_signed_updated_at_is_microseconds_not_seconds() {
        let rec = std::sync::Arc::new(CatalogRecordingRequester::default());
        publish(&rec, vec![(hex_id(0x11), 400)]).unwrap();
        let metadata: AlgorithmLabeler =
            fauna_core::encoding::canonical_decode(&rec.published().metadata_blob).unwrap();
        // 2020-01-01 in µs — any seconds- or millis-since-epoch stamp is far below.
        assert!(
            metadata.updated_at.0 > 1_577_836_800_000_000,
            "updated_at {} is not µs-since-epoch",
            metadata.updated_at.0
        );
    }

    /// The property the whole republish story rests on: the key is a pure,
    /// stable function of (actor secret, factor id).
    #[test]
    fn the_signing_seed_is_stable_per_factor_and_differs_across_factors() {
        assert_eq!(
            labeler_signing_seed(&SECRET, &FACTOR),
            labeler_signing_seed(&SECRET, &FACTOR),
            "a republish must re-derive the SAME key, or the version bump lands nowhere"
        );
        assert_ne!(
            labeler_signing_seed(&SECRET, &FACTOR),
            labeler_signing_seed(&SECRET, &[8u8; 16]),
            "a second factor must publish under its own id — one key per user would \
             cap them at ONE list forever"
        );
        assert_ne!(
            labeler_signing_seed(&SECRET, &FACTOR),
            labeler_signing_seed(&[4u8; 32], &FACTOR),
            "a different actor must derive a different key"
        );
    }

    /// Pins the exact derivation the design ratified, so the streaming form in
    /// [`labeler_signing_seed`] can never silently drift from
    /// `derive_key(context, actor_secret || factor_id)` — every already-published
    /// list's id depends on it.
    #[test]
    fn the_signing_seed_is_the_domain_separated_concatenation() {
        let mut material = [0u8; 48];
        material[..32].copy_from_slice(&SECRET);
        material[32..].copy_from_slice(&FACTOR);
        assert_eq!(
            labeler_signing_seed(&SECRET, &FACTOR),
            blake3::derive_key(LABELER_PUBLISH_CONTEXT, &material),
        );
    }

    /// Pseudonymity: the published id must not be the user's own actor key, nor
    /// the raw secret. Conflating them would tell every subscriber who published.
    #[test]
    fn the_publishing_identity_is_not_the_actors_own_key() {
        let seed = labeler_signing_seed(&SECRET, &FACTOR);
        assert_ne!(
            seed, SECRET,
            "the signing seed must not be the actor secret"
        );
        assert_ne!(
            derived_id(&SECRET, &FACTOR),
            SigningKey::from_bytes(&SECRET).verifying_key().to_bytes(),
            "the labeler id must not be the actor's own public key"
        );
    }

    /// Republish: the catalog reports our derived id live at N, so we must sign
    /// N+1 — the nest enforces monotonicity and would reject anything else.
    #[test]
    fn version_resolution_bumps_past_our_existing_labeler() {
        let rec = std::sync::Arc::new(CatalogRecordingRequester::with_catalog(vec![summary(
            derived_id(&SECRET, &FACTOR),
            4,
        )]));
        let out = publish(&rec, vec![(hex_id(0x11), 400)]).unwrap();
        assert_eq!(out.version, 5);

        let metadata: AlgorithmLabeler =
            fauna_core::encoding::canonical_decode(&rec.published().metadata_blob).unwrap();
        assert_eq!(
            metadata.version, 5,
            "the bumped version must be the SIGNED one, not just the returned one"
        );
    }

    /// A first publish, with an empty catalog.
    #[test]
    fn version_starts_at_one_when_we_have_never_published() {
        let rec = std::sync::Arc::new(CatalogRecordingRequester::default());
        assert_eq!(publish(&rec, vec![(hex_id(0x11), 400)]).unwrap().version, 1);
    }

    /// Version resolution keys on OUR derived id: a busy catalog full of other
    /// publishers' labelers is still a first publish for us.
    #[test]
    fn another_publishers_labeler_does_not_bump_our_version() {
        let rec = std::sync::Arc::new(CatalogRecordingRequester::with_catalog(vec![
            summary([0xAAu8; 32], 9),
            // Our own OTHER factor's list — same actor, different id: it must not
            // bump this factor either.
            summary(derived_id(&SECRET, &[8u8; 16]), 7),
        ]));
        assert_eq!(publish(&rec, vec![(hex_id(0x11), 400)]).unwrap().version, 1);
    }

    /// Duplicates collapse (builder: last-wins), and the reported count is the
    /// post-dedup truth — what the nest will actually materialize.
    #[test]
    fn entry_count_reports_the_post_dedup_truth() {
        let rec = std::sync::Arc::new(CatalogRecordingRequester::default());
        let out = publish(
            &rec,
            vec![
                (hex_id(0x11), 400),
                (hex_id(0x11), 900),
                (hex_id(0x22), 100),
            ],
        )
        .unwrap();
        assert_eq!(out.entry_count, 2, "the input's 3 rows carry only 2 ids");

        let artifact = validate_list_artifact(&rec.published().wasm_bytes).unwrap();
        assert_eq!(artifact.entries.len(), 2);
        assert_eq!(artifact.entries[0].score, 900, "last write wins");
    }

    /// A malformed id is a typed error naming the row — never a silent skip,
    /// which would publish a different list than the user reviewed.
    #[test]
    fn a_bad_hex_post_id_is_a_typed_error_naming_the_row() {
        let rec = std::sync::Arc::new(CatalogRecordingRequester::default());
        let err = publish(
            &rec,
            vec![(hex_id(0x11), 400), ("nothex!!".to_string(), 900)],
        )
        .unwrap_err();
        assert!(
            matches!(err, PublishListError::ContentIdHex { index: 1, .. }),
            "expected a hex error at row 1, got {err:?}"
        );
        assert!(rec.last.lock().unwrap().is_none(), "nothing may reach nest");
    }

    #[test]
    fn a_wrong_length_post_id_is_a_typed_error_naming_the_row() {
        let rec = std::sync::Arc::new(CatalogRecordingRequester::default());
        let err = publish(&rec, vec![(hex::encode([1u8; 16]), 400)]).unwrap_err();
        assert_eq!(err, PublishListError::ContentIdLen { index: 0, len: 16 });
        assert!(rec.last.lock().unwrap().is_none(), "nothing may reach nest");
    }

    /// An out-of-range score is the artifact layer's typed error, raised before
    /// the wire rather than at the nest gate.
    #[test]
    fn an_out_of_range_score_is_refused_before_the_wire() {
        let rec = std::sync::Arc::new(CatalogRecordingRequester::default());
        let err = publish(&rec, vec![(hex_id(0x11), 1001)]).unwrap_err();
        assert!(
            matches!(err, PublishListError::Artifact(_)),
            "expected an artifact error, got {err:?}"
        );
        assert!(rec.last.lock().unwrap().is_none(), "nothing may reach nest");
    }

    // ── v2: publishing the scrubbed MODEL (topic-factors.md § Publishing a
    // trained factor) ────────────────────────────────────────────────────

    use fauna_core::scoring::{
        TEXT_MODEL_ARTIFACT_VERSION, TEXT_MODEL_PUBLISH_MAX_NGRAMS, TEXT_MODEL_PUBLISH_MIN_DOCS,
    };
    use fauna_text_model::publish::scrub_corpus;
    use fauna_text_model::topic::{ExampleLabel, TopicModel};

    fn publish_model(
        rec: &std::sync::Arc<CatalogRecordingRequester>,
        more_docs: u32,
        less_docs: u32,
        ngrams: Vec<(String, u32, u32)>,
    ) -> Result<PublishedModel, PublishModelError> {
        block_on(publish_trained_factor_model(
            rec.clone(),
            &SECRET,
            &FACTOR,
            "Small orange cats",
            more_docs,
            less_docs,
            ngrams,
        ))
    }

    /// The happy path: what reaches the wire is a `text-model` artifact of
    /// `post` content, canonical by the nest gate's own validator.
    #[test]
    fn publish_model_composes_a_canonical_text_model_artifact_over_post_content() {
        let rec = std::sync::Arc::new(CatalogRecordingRequester::default());
        // Out of order on purpose — the builder owns the canonical form.
        let out = publish_model(
            &rec,
            4,
            2,
            vec![
                ("orange".to_string(), 3, 0),
                ("cat".to_string(), 4, 1),
                ("dog".to_string(), 1, 2),
            ],
        )
        .unwrap();

        let req = rec.published();
        assert_eq!(req.artifact_kind, artifact_kind::TEXT_MODEL);
        assert_eq!(req.content_kind, "post", "v2 is public-post-only, like v1");

        let artifact = validate_text_model_artifact(req.wasm_bytes.as_ref())
            .expect("the nest gate's own validator must accept what we signed");
        assert_eq!(artifact.version, TEXT_MODEL_ARTIFACT_VERSION);
        assert_eq!(artifact.name.as_deref(), Some("Small orange cats"));
        assert_eq!(artifact.more_docs, 4);
        assert_eq!(artifact.less_docs, 2);
        let grams: Vec<&str> = artifact.ngrams.iter().map(|n| n.ngram.as_str()).collect();
        assert_eq!(grams, vec!["cat", "dog", "orange"]);

        assert_eq!(out.ngram_count, 3);
        assert_eq!(out.document_count, 6);
        assert_eq!(out.version, 1);

        // The signed metadata binds these exact bytes, and the publisher is the
        // derived per-factor key — never the actor (the pseudonymity property).
        let metadata: AlgorithmLabeler =
            fauna_protocol::decode_strict(req.metadata_blob.as_ref()).expect("metadata decodes");
        verify_labeler_metadata(&metadata, req.wasm_bytes.as_ref())
            .expect("metadata must verify against the artifact");
        assert_eq!(out.labeler_id, derived_id(&SECRET, &FACTOR));
        assert_ne!(
            out.labeler_id, SECRET,
            "a published model carries no author attribution"
        );
    }

    /// ⚠ The List → Model upgrade path: the **same factor** publishes under the
    /// **same labeler id**, one version past whatever kind was live. That is
    /// what lets a subscriber keep their subscription across the upgrade
    /// (§ Publishing: "the kind is per-version"), and it is the precondition for
    /// the frame's List-row withdrawal clause even being reachable.
    #[test]
    fn a_model_publish_is_a_version_bump_of_the_factors_existing_list() {
        let id = derived_id(&SECRET, &FACTOR);
        let rec = std::sync::Arc::new(CatalogRecordingRequester::with_catalog(vec![summary(
            id, 3,
        )]));
        let out = publish_model(&rec, 3, 0, vec![("orange".to_string(), 3, 0)]).unwrap();

        assert_eq!(out.labeler_id, id, "same factor, same publishing identity");
        assert_eq!(out.version, 4, "one past the live List version, not 1");
        assert_eq!(rec.published().artifact_kind, artifact_kind::TEXT_MODEL);

        // And a List publish of the same factor lands on the same id — the two
        // kinds are one artifact lineage, not two.
        let rec2 = std::sync::Arc::new(CatalogRecordingRequester::default());
        assert_eq!(
            publish(&rec2, vec![(hex_id(0x11), 400)])
                .unwrap()
                .labeler_id,
            id
        );
    }

    #[test]
    fn an_empty_vocabulary_is_refused_before_the_wire() {
        let rec = std::sync::Arc::new(CatalogRecordingRequester::default());
        assert_eq!(
            publish_model(&rec, 0, 0, vec![]).unwrap_err(),
            PublishModelError::EmptyVocabulary
        );
        assert!(rec.last.lock().unwrap().is_none(), "nothing may reach nest");
    }

    /// A below-floor n-gram is refused **before** the wire, by the same
    /// validator the nest runs — so the publisher never learns at the gate that
    /// the bytes it signed were out of contract.
    #[test]
    fn a_below_floor_ngram_is_refused_before_the_wire() {
        let rec = std::sync::Arc::new(CatalogRecordingRequester::default());
        let err = publish_model(&rec, 9, 9, vec![("quote".to_string(), 1, 0)]).unwrap_err();
        assert!(
            matches!(err, PublishModelError::Artifact(_)),
            "expected an artifact error, got {err:?}"
        );
        assert!(rec.last.lock().unwrap().is_none(), "nothing may reach nest");
    }

    /// ⚠⚠ THE STRUCTURAL PRIVACY PIN of the v2 design (§ Publishing a trained
    /// factor): **a factor trained with engagement *and* restricted examples
    /// publishes byte-identically to its explicit-public-only twin.**
    ///
    /// Two factors are trained here — one on public examples only, one that
    /// additionally learned from engagement cues and from a restricted post the
    /// publisher can see but the public cannot. Both then publish from the SAME
    /// public corpus (the restricted post is dropped before the scrub, as a real
    /// corpus fetch drops it), and the artifact bytes on the wire must be
    /// identical. Byte equality is the right assertion rather than "no secret
    /// substring appears": it forecloses *every* channel at once, including
    /// count skew, which a substring search would miss entirely.
    /// Marker ids for the six publicly-fetchable examples, and for the one post
    /// only the publisher can see.
    const PUBLIC_IDS: [&str; 6] = ["a0", "a1", "a2", "b0", "b1", "b2"];
    const RESTRICTED_ID: &str = "ff";

    #[test]
    fn an_engagement_and_restricted_trained_factor_publishes_byte_identically() {
        let public_corpus: Vec<(String, ExampleLabel)> = vec![
            (
                "orange cat on a windowsill".to_string(),
                ExampleLabel::MoreLikeThis,
            ),
            (
                "small orange cat with yarn".to_string(),
                ExampleLabel::MoreLikeThis,
            ),
            ("my orange cat naps".to_string(), ExampleLabel::MoreLikeThis),
            (
                "quarterly revenue deck".to_string(),
                ExampleLabel::LessLikeThis,
            ),
            (
                "quarterly earnings call".to_string(),
                ExampleLabel::LessLikeThis,
            ),
            (
                "quarterly budget review".to_string(),
                ExampleLabel::LessLikeThis,
            ),
        ];

        // The lean twin: explicit, public examples only.
        let mut lean = TopicModel::new();
        for (i, (text, label)) in public_corpus.iter().enumerate() {
            lean.train(PUBLIC_IDS[i], text, label.clone());
        }

        // The rich factor: the same public examples, PLUS a restricted post and
        // PLUS engagement training — everything the publisher's private model
        // knows that the public corpus does not.
        let mut rich = TopicModel::new();
        for (i, (text, label)) in public_corpus.iter().enumerate() {
            rich.train(PUBLIC_IDS[i], text, label.clone());
        }
        rich.train(
            RESTRICTED_ID,
            "restricted medical appointment thursday",
            ExampleLabel::MoreLikeThis,
        );
        rich.train_engagement(
            "engagement only observation text",
            None,
            Some(ExampleLabel::MoreLikeThis),
        );
        assert_ne!(
            lean, rich,
            "fixture check: the two private models must genuinely differ"
        );

        // The corpus read, in the shape the real one has: walk the factor's own
        // example markers, "fetch" each marked post from what is PUBLICLY
        // fetchable, and drop every miss. The restricted post is simply absent
        // from this table — which is exactly what a real fetch of it yields for
        // the publishing client's *audience*, and is why the rich factor's extra
        // marker contributes nothing.
        let public_posts: std::collections::BTreeMap<&str, &str> = public_corpus
            .iter()
            .enumerate()
            .map(|(i, (text, _))| (PUBLIC_IDS[i], text.as_str()))
            .collect();

        let publish_from = |model: &TopicModel| {
            let corpus: Vec<(String, ExampleLabel)> = model
                .example_markers()
                .filter_map(|(id, label)| {
                    // Fetch-failed / restricted / deleted ⇒ excluded, never a
                    // best-effort substitute from local state.
                    public_posts.get(id).map(|text| (text.to_string(), label))
                })
                .collect();
            let vocab = scrub_corpus(
                &corpus,
                TEXT_MODEL_PUBLISH_MIN_DOCS,
                TEXT_MODEL_PUBLISH_MAX_NGRAMS,
            );
            let rec = std::sync::Arc::new(CatalogRecordingRequester::default());
            publish_model(
                &rec,
                vocab.more_docs,
                vocab.less_docs,
                vocab
                    .ngrams
                    .iter()
                    .map(|n| (n.ngram.clone(), n.more, n.less))
                    .collect(),
            )
            .unwrap();
            rec.published().wasm_bytes.as_ref().to_vec()
        };

        let lean_bytes = publish_from(&lean);
        let rich_bytes = publish_from(&rich);
        assert_eq!(
            lean_bytes, rich_bytes,
            "the engagement- and restricted-trained factor must publish the \
             SAME bytes as its explicit-public-only twin"
        );

        // Belt and braces on the two leak classes a reader will ask about by
        // name, so a failure diagnoses itself rather than just saying "bytes
        // differ".
        let artifact = validate_text_model_artifact(&rich_bytes).unwrap();
        let grams: Vec<&str> = artifact.ngrams.iter().map(|n| n.ngram.as_str()).collect();
        assert!(
            !grams
                .iter()
                .any(|g| g.contains("medical") || g.contains("restricted")),
            "restricted-post text must never reach the artifact: {grams:?}"
        );
        assert!(
            !grams.iter().any(|g| g.contains("engagement")),
            "engagement-trained text must never reach the artifact: {grams:?}"
        );
        assert!(
            !grams
                .iter()
                .any(|g| g.contains(RESTRICTED_ID) || PUBLIC_IDS.iter().any(|id| g.contains(id))),
            "example marker ids must never reach the artifact: {grams:?}"
        );
    }
}
