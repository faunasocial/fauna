//! The archive folder's at-rest records (`archive-import.md` § Storage) —
//! every one canonical DAG-CBOR through `fauna_cbor`, evolved additively.
//!
//! The folder is the session: there is deliberately no nest-side
//! `import_sessions` twin for archives (§ Don't do these), because the nest
//! cannot read the archive and a plaintext progress row would only add to the
//! plaintext floor. State inside the sealed folder is strictly more private
//! and yields cross-device restart-resume for free.
//!
//! Field names are part of the at-rest contract from the first commit: add
//! `#[serde(default)]` fields, never rename or remove one
//! (`version-compatibility.md` § I4).
//!
//! **A newer build's state.** The folder is resumed by any of the owner's
//! devices, so a state one build wrote is read by another. Every enum in
//! `state/import.cbor` carries an open arm, and [`ImportState`] /
//! [`ImportScope`] carry the fields this build does not name in `extra`
//! (`transport.md` § Schema and forward-compat discipline → *Rule 3 in
//! full*): such a state always decodes, so its imported map still feeds
//! dedup. But a state holding anything this build cannot read
//! ([`ImportState::holds_unknown`]) is **not resumable by this build and is
//! never rewritten by it** — a resume or a cancel would rewrite it whole, and
//! this build cannot know what the newer value asks of the run.

use std::collections::BTreeMap;

use fauna_archive::{ArchiveSummary, Category, ExternalActorRef, ExternalId, Platform};
use fauna_core::carried::CarriedValue;
use fauna_core::data::Timestamp;
use serde::{Deserialize, Serialize};

pub const MARKER_PATH: &str = "fauna-archive.cbor";
pub const STATE_PATH: &str = "state/import.cbor";
pub const SUMMARY_PATH: &str = "model/summary.cbor";

/// `raw/<original filename>` — the untouched export zip, kept forever.
pub fn raw_path(file_name: &str) -> String {
    format!("raw/{file_name}")
}

/// `model/<category>.cbor` — the normalized model one category at a time.
pub fn model_path(category: &Category) -> String {
    format!("model/{}.cbor", category.token())
}

/// `fauna-archive.cbor` — how a later session recognizes an archive folder.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArchiveMarker {
    pub platform: Platform,
    pub owner: ExternalActorRef,
    /// Hex of 32 random bytes minted at Start.
    pub import_id: String,
    pub parser_version: u32,
    pub raw_file_name: String,
    pub created_at: Timestamp,
    /// The raw zip's byte length and BLAKE3 (hex), recorded once the raw
    /// upload has read it whole. A resume that is handed an archive back
    /// checks both, so an import can never continue over a different export
    /// than the one its model files came from (§ Storage — the zip is the
    /// folder's ground truth, and media is read from it by member path). A
    /// marker from before either field existed reads as unknown, and an
    /// unknown value is not checked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_len: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_blake3: Option<String>,
}

impl ArchiveMarker {
    /// Whether the marker names a platform this build cannot (a newer
    /// parser's) — such a folder is not resumable by this build.
    pub fn holds_unknown(&self) -> bool {
        !self.platform.is_known() || !self.owner.platform.is_known()
    }
}

/// The at-rest twin of [`crate::snapshot::AudienceMode`] — kept separate so
/// the rendered enum can gain UI-only variants without touching the folder's
/// contract, and so this one's open arm never becomes a case an app renders.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StoredAudienceMode {
    Original,
    OnlyMe,
    /// A mode a newer build stored that this one cannot name — the carrying
    /// open arm (`transport.md` § *Rule 3 in full*). It holds the exact
    /// string read and re-emits it, and it **maps as [`Self::OnlyMe`]**
    /// (`audience::map_audience`) — the most restrictive mode, never the
    /// original audience. A state holding one is not resumable by this build
    /// anyway ([`ImportState::holds_unknown`]).
    #[serde(untagged)]
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImportScope {
    pub categories: Vec<Category>,
    pub audience_mode: StoredAudienceMode,
    /// Inclusive lower bound: the first instant of the `since` day.
    pub date_from: Option<Timestamp>,
    /// EXCLUSIVE upper bound: midnight AFTER the `until` day, so the whole
    /// day the user named is in range (`archive-import.md` § The wizard and
    /// its machine, step 3). Stored under its original key; a run started
    /// before this rule holds the `until` day's own midnight here and resumes
    /// under that narrower bound — accepted, the state is client-local.
    #[serde(rename = "date_to")]
    pub date_until_exclusive: Option<Timestamp>,
    /// Fields a newer build added that this one does not name, kept so the
    /// scope re-encodes as read (`transport.md` § Schema and forward-compat
    /// discipline, rule 4). A non-empty `extra` makes the state not resumable
    /// by this build ([`ImportState::holds_unknown`]).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, CarriedValue>,
}

impl ImportScope {
    /// Whether an archive timestamp falls inside the Scope step's range.
    pub(crate) fn admits_date(&self, at: Timestamp) -> bool {
        self.date_from.is_none_or(|from| at >= from)
            && self.date_until_exclusive.is_none_or(|until| at < until)
    }
}

/// Where the run is — the checkpoint a resume continues from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImportPhase {
    RawUpload,
    Model,
    /// Authoring `category`; the next record to consider is index
    /// `next_index` in that category's model file.
    Authoring {
        category: Category,
        next_index: u64,
    },
    Finished,
    Cancelled,
    /// A phase a newer build wrote that this one cannot name, carried whole
    /// (`transport.md` § *Rule 3 in full*). It is neither finished nor
    /// resumable here: a state in it is never resumed, cancelled or rewritten
    /// by this build ([`ImportState::holds_unknown`]).
    #[serde(untagged)]
    Unknown(CarriedValue),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImportTarget {
    Post {
        post_id: String,
    },
    Event {
        uid_hash: String,
    },
    /// A target kind a newer build mapped an import to, carried whole
    /// (`transport.md` § *Rule 3 in full*). Its record's `external_id` still
    /// feeds dedup — what was imported stays imported — and nothing else
    /// reads the target.
    #[serde(untagged)]
    Unknown(CarriedValue),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImportedRecord {
    pub external_id: ExternalId,
    pub target: ImportTarget,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkipEntry {
    pub category: Category,
    pub external_id: Option<ExternalId>,
    pub reason: String,
}

/// A gated post built and written to the folder BEFORE its
/// `fauna.posts.create` (§ The wizard and its machine, step 5 — the
/// checkpoint). A crash between the create and the record's checkpoint then
/// replays exactly these bytes: the sealed body's store and `posts.create`
/// are both content-addressed and idempotent, so a create that did land is a
/// no-op and one that did not lands now — one post either way. Re-*authoring*
/// instead would seal the body under a fresh nonce into a second, different
/// post the user could only delete by hand. Public posts and events need no
/// write-ahead: their re-authoring is byte-identical (a plain envelope has no
/// nonce anywhere, media is content-addressed) and the nest upserts by hash.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InFlightPost {
    pub external_id: ExternalId,
    /// The `fauna.posts.create` body.
    #[serde(with = "serde_bytes")]
    pub post_bytes: Vec<u8>,
    /// The sealed `PostBody` blob the post's `encrypted_ref` names.
    #[serde(with = "serde_bytes")]
    pub sealed_body: Vec<u8>,
}

/// `state/import.cbor`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImportState {
    pub import_id: String,
    pub scope: ImportScope,
    pub phase: ImportPhase,
    /// The ExternalId → Fauna id map (a `Vec`, not a map: DAG-CBOR keys are strings).
    pub imported: Vec<ImportedRecord>,
    pub skipped: Vec<SkipEntry>,
    pub updated_at: Timestamp,
    /// The gated post whose bytes are written ahead of its create, while its
    /// create is unconfirmed (additive — absent on every state from before
    /// the write-ahead existed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_flight: Option<InFlightPost>,
    /// Fields a newer build added that this one does not name (as
    /// [`ImportScope::extra`]).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, CarriedValue>,
}

impl ImportState {
    pub fn is_finished(&self) -> bool {
        matches!(self.phase, ImportPhase::Finished | ImportPhase::Cancelled)
    }

    /// Whether the state holds any value this build cannot read — an unknown
    /// arm of any of its enums, or a field in either `extra`. Such a state is
    /// still read for dedup (its imported map is all `dedup_ids` takes), but
    /// it is **not resumable by this build and never rewritten by it**: the
    /// hydrate passes over it, and `checkpoint` refuses to write it
    /// (`run.rs`).
    pub fn holds_unknown(&self) -> bool {
        let id_unknown = |id: &ExternalId| !id.platform.is_known() || !id.kind.is_known();
        let scope = &self.scope;
        !self.extra.is_empty()
            || !scope.extra.is_empty()
            || matches!(scope.audience_mode, StoredAudienceMode::Other(_))
            || scope.categories.iter().any(|c| !c.is_known())
            || match &self.phase {
                ImportPhase::Unknown(_) => true,
                ImportPhase::Authoring { category, .. } => !category.is_known(),
                _ => false,
            }
            || self
                .imported
                .iter()
                .any(|r| matches!(r.target, ImportTarget::Unknown(_)) || id_unknown(&r.external_id))
            || self
                .skipped
                .iter()
                .any(|s| !s.category.is_known() || s.external_id.as_ref().is_some_and(id_unknown))
            || self
                .in_flight
                .as_ref()
                .is_some_and(|f| id_unknown(&f.external_id))
    }

    pub fn has(&self, id: &ExternalId) -> bool {
        self.imported.iter().any(|r| &r.external_id == id)
    }
}

/// `model/summary.cbor` is the parser's `ArchiveSummary` verbatim.
pub type StoredSummary = ArchiveSummary;

/// The open arms of `state/import.cbor` (`transport.md` § Schema and
/// forward-compat discipline → *Rule 3 in full*): a newer build is modelled
/// as test-only twins with one value this build lacks, encoded in the
/// folder's own canonical DAG-CBOR and decoded with the real types.
#[cfg(test)]
pub(crate) mod unknown_arm_tests {
    use super::*;
    use fauna_archive::EntityKind;

    #[derive(Serialize, Clone)]
    #[serde(rename_all = "snake_case")]
    enum NewerCategory {
        Posts,
        Reels,
    }

    #[derive(Serialize, Clone)]
    #[serde(rename_all = "snake_case")]
    enum NewerMode {
        Original,
        FriendsOfFriends,
    }

    #[derive(Serialize, Clone)]
    #[serde(rename_all = "snake_case")]
    enum NewerPhase {
        Authoring {
            category: NewerCategory,
            next_index: u64,
        },
        Reindexing {
            pass: u64,
            #[serde(with = "serde_bytes")]
            cursor: Vec<u8>,
        },
    }

    #[derive(Serialize, Clone)]
    #[serde(rename_all = "snake_case")]
    enum NewerTarget {
        Post { post_id: String },
        Album { album_id: String, members: Vec<i64> },
    }

    #[derive(Serialize, Clone)]
    struct NewerScope {
        categories: Vec<NewerCategory>,
        audience_mode: NewerMode,
        date_from: Option<Timestamp>,
        date_to: Option<Timestamp>,
        #[serde(skip_serializing_if = "Option::is_none")]
        region: Option<String>,
    }

    #[derive(Serialize, Clone)]
    struct NewerRecord {
        external_id: ExternalId,
        target: NewerTarget,
    }

    #[derive(Serialize, Clone)]
    struct NewerState {
        import_id: String,
        scope: NewerScope,
        phase: NewerPhase,
        imported: Vec<NewerRecord>,
        skipped: Vec<SkipEntry>,
        updated_at: Timestamp,
        #[serde(skip_serializing_if = "Option::is_none")]
        resume_token: Option<u64>,
    }

    fn post_id(id: &str) -> ExternalId {
        ExternalId::native(Platform::Facebook, EntityKind::Post, id)
    }

    /// A newer writer's state holding only values this build names.
    fn known() -> NewerState {
        NewerState {
            import_id: "ab".repeat(32),
            scope: NewerScope {
                categories: vec![NewerCategory::Posts],
                audience_mode: NewerMode::Original,
                date_from: None,
                date_to: Some(Timestamp(5)),
                region: None,
            },
            phase: NewerPhase::Authoring {
                category: NewerCategory::Posts,
                next_index: 3,
            },
            imported: vec![NewerRecord {
                external_id: post_id("p1"),
                target: NewerTarget::Post {
                    post_id: "aa".into(),
                },
            }],
            skipped: vec![],
            updated_at: Timestamp(9),
            resume_token: None,
        }
    }

    /// The bytes of a newer build's unfinished state: a phase this build
    /// cannot name, a scope field it lacks — and `imported`, which dedup must
    /// still read.
    pub(crate) fn newer_state_bytes(imported: &[ImportedRecord]) -> Vec<u8> {
        let mut state = known();
        state.phase = NewerPhase::Reindexing {
            pass: 2,
            cursor: vec![1, 2, 3],
        };
        state.scope.region = Some("eu".into());
        state.imported = imported
            .iter()
            .map(|r| NewerRecord {
                external_id: r.external_id.clone(),
                target: match &r.target {
                    ImportTarget::Post { post_id } => NewerTarget::Post {
                        post_id: post_id.clone(),
                    },
                    other => panic!("the twin covers posts only: {other:?}"),
                },
            })
            .collect();
        fauna_cbor::encode_canonical(&state).unwrap()
    }

    /// Decodes with the real type, asserts the imported map is readable for
    /// dedup and the state re-encodes byte-identically; returns it for the
    /// caller's own checks.
    fn decodes_and_round_trips(newer: &NewerState) -> ImportState {
        let bytes = fauna_cbor::encode_canonical(newer).unwrap();
        let state: ImportState = fauna_cbor::decode_strict(&bytes).expect("the state decodes");
        assert_eq!(
            state
                .imported
                .iter()
                .map(|r| r.external_id.clone())
                .collect::<Vec<_>>(),
            newer
                .imported
                .iter()
                .map(|r| r.external_id.clone())
                .collect::<Vec<_>>(),
            "the imported map is readable for dedup"
        );
        assert_eq!(
            fauna_cbor::encode_canonical(&state).unwrap(),
            bytes,
            "the carried state re-encodes as read"
        );
        state
    }

    /// The twin mirrors the real shape: with nothing unknown it decodes into
    /// a state this build may resume.
    #[test]
    fn a_state_of_known_values_is_resumable() {
        let state = decodes_and_round_trips(&known());
        assert!(!state.holds_unknown());
        assert!(state.extra.is_empty() && state.scope.extra.is_empty());
    }

    #[test]
    fn an_unknown_phase_is_carried_neither_finished_nor_resumable() {
        let mut newer = known();
        newer.phase = NewerPhase::Reindexing {
            pass: 1,
            cursor: vec![0, 255],
        };
        let state = decodes_and_round_trips(&newer);
        assert!(matches!(state.phase, ImportPhase::Unknown(_)));
        assert!(!state.is_finished());
        assert!(state.holds_unknown());
    }

    #[test]
    fn an_unknown_category_in_the_phase_or_scope_is_carried_and_not_resumable() {
        let mut newer = known();
        newer.phase = NewerPhase::Authoring {
            category: NewerCategory::Reels,
            next_index: 0,
        };
        let state = decodes_and_round_trips(&newer);
        assert!(matches!(
            &state.phase,
            ImportPhase::Authoring { category: Category::Other(t), .. } if t == "reels"
        ));
        assert!(state.holds_unknown());

        let mut newer = known();
        newer.scope.categories.push(NewerCategory::Reels);
        let state = decodes_and_round_trips(&newer);
        assert_eq!(
            state.scope.categories,
            vec![Category::Posts, Category::Other("reels".into())]
        );
        assert!(state.holds_unknown());
    }

    #[test]
    fn an_unknown_audience_mode_is_carried_and_not_resumable() {
        let mut newer = known();
        newer.scope.audience_mode = NewerMode::FriendsOfFriends;
        let state = decodes_and_round_trips(&newer);
        assert_eq!(
            state.scope.audience_mode,
            StoredAudienceMode::Other("friends_of_friends".into())
        );
        assert!(state.holds_unknown());
    }

    #[test]
    fn an_unknown_target_is_carried_and_its_record_still_dedups() {
        let mut newer = known();
        newer.imported.push(NewerRecord {
            external_id: post_id("p2"),
            target: NewerTarget::Album {
                album_id: "x".into(),
                members: vec![-1, 2],
            },
        });
        let state = decodes_and_round_trips(&newer);
        assert!(matches!(state.imported[1].target, ImportTarget::Unknown(_)));
        assert!(state.has(&post_id("p2")));
        assert!(state.holds_unknown());
    }

    #[test]
    fn unknown_fields_of_the_state_and_scope_are_carried_and_not_resumable() {
        let mut newer = known();
        newer.scope.region = Some("eu".into());
        let state = decodes_and_round_trips(&newer);
        assert!(state.scope.extra.contains_key("region"));
        assert!(state.holds_unknown());

        let mut newer = known();
        newer.resume_token = Some(7);
        let state = decodes_and_round_trips(&newer);
        assert!(state.extra.contains_key("resume_token"));
        assert!(state.holds_unknown());
    }

    #[test]
    fn an_unknown_platform_or_kind_inside_an_id_is_not_resumable() {
        let mut newer = known();
        newer.imported[0].external_id = ExternalId {
            platform: Platform::Other("mastodon".into()),
            kind: EntityKind::Post,
            id: "m1".into(),
        };
        let state = decodes_and_round_trips(&newer);
        assert!(state.holds_unknown());

        let mut newer = known();
        newer.skipped.push(SkipEntry {
            category: Category::Posts,
            external_id: Some(ExternalId {
                platform: Platform::Facebook,
                kind: EntityKind::Other("story".into()),
                id: "s1".into(),
            }),
            reason: "r".into(),
        });
        let state = decodes_and_round_trips(&newer);
        assert!(state.holds_unknown());
    }

    /// A marker naming a platform a newer parser added is not resumable.
    #[test]
    fn a_marker_with_an_unknown_platform_is_not_resumable() {
        let owner = ExternalActorRef::new(Platform::Facebook, None, "Owner");
        let mut marker = ArchiveMarker {
            platform: Platform::Facebook,
            owner,
            import_id: "00".repeat(32),
            parser_version: 1,
            raw_file_name: "a.zip".into(),
            created_at: Timestamp(1),
            raw_len: None,
            raw_blake3: None,
        };
        assert!(!marker.holds_unknown());
        marker.platform = Platform::Other("mastodon".into());
        assert!(marker.holds_unknown());
    }
}
