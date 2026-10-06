//! The trained-topic **rows** — the model-plane half of the trained-topic
//! lifecycle (`topic-factors.md` § Authoring surface & picker + § Wire &
//! registry).
//!
//! A trained topic is two records in two places: the **registry entry** (its
//! id, its user-chosen name — the `personalization` record on the account
//! plane, never sent to the nest) and the **model row** (the sealed n-gram blob
//! plus its advisory `sample_count` — keyed by the derived `topic:<hex>`). The
//! account plane's `fauna_account_plane::preference_surfaces` owns the registry
//! writes and drives this type for the model plane: [`TrainedTopics::rows_from`]
//! reads each factor's `sample_count` (no unseal: the count is the nest-stored
//! advisory the manager refreshes on every train), and
//! [`TrainedTopics::delete_model`] is a delete's second call, after the
//! registry removal.

use fauna_i18n::strings::personalization;
use fauna_protocol::{RpcErrorClass, RpcRequester};

use crate::PersonalizationClient;

/// One row of the Trained-topics facet: the registry entry plus its advisory
/// example count off the model plane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrainedTopicRow {
    /// The factor's 16-byte id (the registry's stable handle; rename never
    /// changes it, so compositions keep resolving).
    pub id: Vec<u8>,
    /// The user's chosen display name. Sealed — the nest never sees it.
    pub name: String,
    /// The composition key (`topic:<hex>`) the picker offers and the train
    /// gestures target. `None` only for a corrupt (non-16-byte) id, which has
    /// no addressable model.
    pub factor_key: Option<String>,
    /// Advisory count of explicit examples trained into this factor, as stored
    /// nest-side. `0` for a factor never trained (no model row yet).
    pub example_count: u32,
    /// The per-factor Layer-A opt-in ("Learn from my activity"): when on, the
    /// feed's engagement cues weak-train this factor
    /// (topic-factors.md § Training signals). Off per v1 until the user flips
    /// the row's toggle.
    pub learn_from_engagement: bool,
}

/// What can go wrong driving the lifecycle. Typed rather than pre-formatted:
/// each shell localizes (the cap message names the limit, so it needs the
/// number, not a sentence).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrainedTopicsError {
    /// The registry is full — the client-side twin of the nest's create-cap, so
    /// the user hits the limit at *create* (registry still consistent) rather
    /// than at first train. Carries
    /// [`fauna_protocol::personalization::TRAINED_FACTORS_MAX`].
    Cap(usize),
    /// The name was empty (or all whitespace). Distinct from [`Self::Cap`] on
    /// purpose: `add_trained_factor` answers `None` to both, and a shell that
    /// collapsed them would tell a user who submitted an empty box that they
    /// had run out of topics. Shells guard this at submit; the shared layer
    /// still refuses to mint an unnameable row.
    BlankName,
    /// The account store could not be read or written.
    Config(String),
    /// The model plane (`fauna.personalization.model.*`) rejected a call.
    Model(String),
}

impl core::fmt::Display for TrainedTopicsError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Cap(max) => write!(f, "at most {max} trained topics"),
            Self::BlankName => write!(f, "a trained topic needs a name"),
            Self::Config(e) => write!(f, "config: {e}"),
            Self::Model(e) => write!(f, "model: {e}"),
        }
    }
}

impl TrainedTopicsError {
    /// The finished, user-facing sentence for this rejection — the **one**
    /// `TrainedTopicsError → text` map, for the shells that consume this crate
    /// as Rust (linux and tui).
    ///
    /// The typed-not-pre-formatted contract above is unchanged and is what
    /// makes this a *method* rather than a `Display` rewrite: the cap message
    /// must name the limit, so the boundary keeps carrying the number
    /// (`topic-factors.md` § the trained-topic lifecycle). The five non-Rust
    /// apps still localize on their own side — they never see this type, only
    /// the machine-readable door enums (`FfiTrainedTopicsError`,
    /// `topics_err_to_js`) — so this map serves exactly the two shells that
    /// compile `fauna-i18n` directly, the same split
    /// [`crate::topics`]'s sibling controls already use.
    ///
    /// It exists because those two shells had written it twice **and
    /// disagreed**: tui returned the bare `msg` for `Config`/`Model` while
    /// linux fell through to [`Display`](core::fmt::Display), pasting this
    /// crate's internal `"config: "` / `"model: "` discriminant prefixes onto
    /// a sentence a user reads. The bare `msg` is the shape the FFI door
    /// (`General { msg }`) and the wasm door already hand their shells, so it
    /// is three surfaces to one, not a coin flip.
    ///
    /// `BlankName` deliberately keeps the crate's own sentence: no dedicated
    /// string exists for it and none is invented here — the same fallback
    /// apple's `mapPublishError` and android's raw `e.message` already take.
    /// Near-unreachable anyway, since every shell refuses a blank buffer
    /// before the round trip.
    pub fn localized(&self) -> String {
        match self {
            Self::Cap(max) => personalization::trained_factor_cap(&max.to_string()),
            Self::BlankName => self.to_string(),
            Self::Config(msg) | Self::Model(msg) => msg.clone(),
        }
    }
}

impl core::error::Error for TrainedTopicsError {}

/// Drives the trained-topic lifecycle over one actor's two planes.
pub struct TrainedTopics<R: RpcRequester + Clone> {
    nest: R,
}

impl<R: RpcRequester + Clone> TrainedTopics<R>
where
    R::Error: RpcErrorClass,
{
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// The facet's rows over a registry the caller already holds: every
    /// registry entry, each carrying the advisory example count its model row
    /// reports. A factor whose id is corrupt has no addressable model — it
    /// renders with count 0 rather than vanishing, so the user can still see
    /// (and delete) it.
    ///
    /// The account plane (`fauna_account_plane::preference_surfaces`) reads the
    /// `personalization` sub-record off the replica's own store and hands it
    /// here; the model-plane leg stays a nest call, because the model blob is a
    /// separate plane the account store does not carry.
    pub async fn rows_from(
        &self,
        registry: &fauna_core::data::PersonalizationConfig,
    ) -> Result<Vec<TrainedTopicRow>, TrainedTopicsError> {
        let pc = PersonalizationClient::new(self.nest.clone());
        let mut rows = Vec::new();
        for meta in &registry.trained_factors {
            let factor_key = meta.factor_key();
            let example_count = match &factor_key {
                Some(key) => {
                    pc.model_fetch(key.clone())
                        .await
                        .map_err(|e| TrainedTopicsError::Model(e.to_string()))?
                        .sample_count
                }
                None => 0,
            };
            rows.push(TrainedTopicRow {
                id: meta.id.clone(),
                name: meta.name.clone(),
                factor_key,
                example_count,
                learn_from_engagement: meta.learn_from_engagement,
            });
        }
        Ok(rows)
    }

    /// The model-plane leg of a topic delete, over a key the caller's own
    /// registry removal produced — `None` (nothing matched, or the entry's id
    /// was corrupt) is a no-op, since there is nothing addressable nest-side.
    ///
    /// Public so the account-plane rail
    /// (`fauna_sync_engine::preference_surfaces::delete_trained_topic`) pairs
    /// its registry removal with the same call in the same order: registry
    /// first, model second. Reversing it would risk a live registry entry
    /// pointing at a model the nest already dropped, where this order's worst
    /// case is an orphaned sealed blob nothing references.
    pub async fn delete_model(&self, key: Option<String>) -> Result<(), TrainedTopicsError> {
        if let Some(key) = key {
            PersonalizationClient::new(self.nest.clone())
                .model_delete(key)
                .await
                .map_err(|e| TrainedTopicsError::Model(e.to_string()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::personalization::TRAINED_FACTORS_MAX;
    use fauna_protocol::personalization::{
        KIND_MODEL_DELETE, KIND_MODEL_FETCH, KIND_MODEL_PUT, PersonalizationModelDeleteReply,
        PersonalizationModelDeleteRequest, PersonalizationModelFetchReply,
        PersonalizationModelFetchRequest, PersonalizationModelPutReply,
        PersonalizationModelPutRequest,
    };
    use serde_bytes::ByteBuf;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    #[derive(Debug)]
    struct FakeError(String, Option<fauna_protocol::RpcError>);
    impl FakeError {
        fn msg(m: String) -> Self {
            Self(m, None)
        }
    }
    impl core::fmt::Display for FakeError {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(f, "{}", self.0)
        }
    }
    impl core::error::Error for FakeError {}
    impl fauna_protocol::RpcErrorClass for FakeError {
        fn is_rejection(&self) -> bool {
            self.1.is_some()
        }
        fn as_rpc_error(&self) -> Option<&fauna_protocol::RpcError> {
            self.1.as_ref()
        }
    }

    /// factor key → (sealed blob, advisory sample_count) — one nest-side model row.
    type ModelRows = Arc<Mutex<HashMap<String, (Vec<u8>, u32)>>>;

    /// Serves the `personalization.model.*` rows, so a test can assert what a
    /// read or a delete did on the model plane.
    #[derive(Default, Clone)]
    struct FakeNest {
        models: ModelRows,
        /// Every model-plane kind seen, in order — the pairing assertions read
        /// this to prove a delete reached the nest (and a create did not).
        model_calls: Arc<Mutex<Vec<String>>>,
    }

    impl FakeNest {
        fn seed_model(&self, factor: &str, sample_count: u32) {
            self.models
                .lock()
                .unwrap()
                .insert(factor.to_string(), (vec![9u8; 8], sample_count));
        }
        fn model_keys(&self) -> Vec<String> {
            let mut k: Vec<_> = self.models.lock().unwrap().keys().cloned().collect();
            k.sort();
            k
        }
        fn model_calls(&self) -> Vec<String> {
            self.model_calls.lock().unwrap().clone()
        }
    }

    impl RpcRequester for FakeNest {
        type Error = FakeError;

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
            if kind.starts_with("fauna.personalization.") {
                self.model_calls.lock().unwrap().push(kind.to_string());
            }
            let reply = match kind {
                KIND_MODEL_FETCH => {
                    let req: PersonalizationModelFetchRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode model fetch");
                    let row = self.models.lock().unwrap().get(&req.factor).cloned();
                    let (blob, sample_count) = match row {
                        Some((b, c)) => (Some(ByteBuf::from(b)), c),
                        // An absent row is "never trained", NOT an error.
                        None => (None, 0),
                    };
                    fauna_protocol::encode_canonical(&PersonalizationModelFetchReply {
                        extra: Default::default(),
                        sealed_blob: blob,
                        sample_count,
                        updated_at: 0,
                    })
                }
                KIND_MODEL_PUT => {
                    let req: PersonalizationModelPutRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode model put");
                    self.models
                        .lock()
                        .unwrap()
                        .insert(req.factor, (req.sealed_blob.into_vec(), req.sample_count));
                    fauna_protocol::encode_canonical(&PersonalizationModelPutReply {
                        extra: Default::default(),
                        status: "ok".to_string(),
                    })
                }
                KIND_MODEL_DELETE => {
                    let req: PersonalizationModelDeleteRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode model delete");
                    let deleted = self.models.lock().unwrap().remove(&req.factor).is_some();
                    fauna_protocol::encode_canonical(&PersonalizationModelDeleteReply {
                        extra: Default::default(),
                        status: "ok".to_string(),
                        deleted,
                    })
                }
                other => return Err(FakeError::msg(format!("unexpected kind {other}"))),
            }
            .expect("encode reply");
            Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
        }
    }

    fn topics(nest: FakeNest) -> TrainedTopics<FakeNest> {
        TrainedTopics::new(nest)
    }

    /// A registry holding one named factor, minted as every writer mints one.
    fn registry_with(name: &str) -> fauna_core::data::PersonalizationConfig {
        let mut registry = fauna_core::data::PersonalizationConfig::default();
        crate::topics::tests::mint(&mut registry, name);
        registry
    }

    pub(super) fn mint(registry: &mut fauna_core::data::PersonalizationConfig, name: &str) {
        fauna_client_config::preference_records::add_trained_factor(registry, name)
            .expect("a factor mints");
    }

    use fauna_client_testkit::block_on as block;

    #[test]
    fn an_empty_registry_has_no_rows() {
        let t = topics(FakeNest::default());
        let rows = block(t.rows_from(&Default::default())).unwrap();
        assert_eq!(rows, vec![]);
    }

    #[test]
    fn a_fresh_factor_reads_back_with_a_derived_key_and_no_examples() {
        let nest = FakeNest::default();
        let t = topics(nest.clone());

        let rows = block(t.rows_from(&registry_with("Cats"))).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Cats");
        assert_eq!(rows[0].id.len(), 16);
        let key = rows[0]
            .factor_key
            .clone()
            .expect("a fresh id derives a key");
        assert!(key.starts_with("topic:") && key.len() == 6 + 32, "{key}");
        // A never-trained topic has no model row — the first train mints it.
        assert_eq!(rows[0].example_count, 0);
        assert_eq!(nest.model_keys(), Vec::<String>::new());
    }

    #[test]
    fn rows_report_the_advisory_example_count_off_the_model_plane() {
        let nest = FakeNest::default();
        let t = topics(nest.clone());
        let registry = registry_with("Cats");
        let key = block(t.rows_from(&registry)).unwrap()[0]
            .factor_key
            .clone()
            .unwrap();

        // The manager writes this count on every train; the facet reads it back
        // with no unseal.
        nest.seed_model(&key, 7);
        let rows = block(t.rows_from(&registry)).unwrap();
        assert_eq!(rows[0].example_count, 7);
    }

    #[test]
    fn delete_model_drops_the_model_row_and_a_missing_key_is_a_no_op() {
        let nest = FakeNest::default();
        let t = topics(nest.clone());
        nest.seed_model("topic:aa", 3);
        block(t.delete_model(Some("topic:aa".into()))).unwrap();
        assert_eq!(nest.model_keys(), Vec::<String>::new());

        let before = nest.model_calls().len();
        block(t.delete_model(None)).unwrap();
        assert_eq!(nest.model_calls().len(), before, "no key, no call");
    }

    // ── `localized` — the one error → sentence map ───────────────────

    /// The cap arm names the limit. This is the whole reason the boundary
    /// stays typed instead of pre-formatted, so it is the arm worth pinning:
    /// the number must reach the sentence.
    #[test]
    fn the_cap_sentence_carries_the_limit() {
        let msg = TrainedTopicsError::Cap(32).localized();
        assert!(msg.contains("32"), "cap sentence lost the limit: {msg}");
    }

    /// **The drift this map was written to end.** `Config`/`Model` carry a
    /// message a user reads; `Display` prefixes them with this crate's own
    /// `"config: "` / `"model: "` discriminant, which linux's catch-all arm
    /// used to paste straight into the page's `error-message` while tui and
    /// both doors handed back the bare text. Bare wins — three surfaces to
    /// one — and a future catch-all arm re-introducing the prefix fails here.
    #[test]
    fn no_internal_discriminant_prefix_reaches_the_user() {
        for e in [
            TrainedTopicsError::Config("the sealed blob would not open".into()),
            TrainedTopicsError::Model("the plane refused the put".into()),
        ] {
            let msg = e.localized();
            assert!(
                !msg.starts_with("config: ") && !msg.starts_with("model: "),
                "internal prefix leaked into user-facing text: {msg}"
            );
            assert!(
                e.to_string().ends_with(&msg),
                "localized text should be the Display sentence minus its prefix: {msg}"
            );
        }
    }

    /// A blank name keeps the crate's own sentence — no i18n string exists for
    /// it, and none is invented here. It must not collapse onto the cap's.
    #[test]
    fn a_blank_name_localizes_to_its_own_sentence() {
        let blank = TrainedTopicsError::BlankName.localized();
        assert_eq!(blank, TrainedTopicsError::BlankName.to_string());
        assert_ne!(
            blank,
            TrainedTopicsError::Cap(TRAINED_FACTORS_MAX).localized()
        );
    }
}
