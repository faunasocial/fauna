//! The **T1 body-rendered browse trigger** — the shared observation intake
//! that admits browse content to the account's data plane.
//!
//! Owner: `docs/goal/architecture/account-data-plane.md` § The replica
//! boundary (R1 (account-data-plane.md § The ratified decisions) + T1, the producer decomposition). R1 admits an item three
//! ways; the two non-observational classes are handled arithmetically by
//! [`crate::seen_set_producer`]. This module is the third: **observation**,
//! and T1 fixes what counts as one — *"a browse item enters the seen-set when
//! its body is materialized for display — the record's content is handed to a
//! visible view — never when its existence merely transits a list buffer, a
//! virtualized scroll's overscan, or a prefetch."*
//!
//! # The division this module exists to hold
//!
//! The charter's producer decomposition splits the rule in two, and the split
//! is the whole point: **this intake owns the entire rule** — browse
//! classification, coordinate resolution, dedup against the merged entry, the
//! class-2 put — while an app's render layer reports only the one platform
//! fact shared Rust cannot know, *"this body was handed to a visible view"*
//! ([`Observation`]). So all 7 apps inherit one rule and no app
//! hand-implements it; an app that reports honestly cannot get the boundary
//! wrong, and an app that reports a body it never showed is the only way to
//! corrupt the set — which is why [`Observation`] is deliberately not
//! constructible from a list index or a prefetch cursor, only from a record
//! the reporter is asserting it displayed.
//!
//! The reporting side of that division follows `render-model.md`'s D3
//! reveal-state shape: **manager-reported** where the manager owns the moment
//! (a thread opened, a preview expanded), **shell-reported** for scroll
//! visibility, with overscan excluded *by the reporter* — the shell is the
//! only layer that knows which of its realized rows are actually on screen.
//!
//! # Itemized, never watermarked
//!
//! A [`SeenWatermark`](fauna_core::seen_set::SeenWatermark) asserts the whole
//! prefix at-or-below it was observed. Browse content earns no such licence —
//! a user who reads one message in a channel has observed exactly that
//! message — so this producer only ever calls
//! [`SeenScopeSet::insert_ref`], and a browse scope's entry stays a set of
//! [`SeenRef`](fauna_core::seen_set::SeenRef)s.
//!
//! # Unresolvable observations are dropped, never queued
//!
//! An observation whose coordinate the local journal cannot yet resolve (the
//! body came from a hydration path that outran the feed walk) is **dropped**
//! and re-recorded on a later render. The grow-only set makes the retry free,
//! and it is the reason this module holds no pending state: CID-keyed pending
//! state would be a second, unsynced boundary structure with its own
//! eviction question, which the charter forecloses.

use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::ItemRef;
use fauna_core::data::ContentHash;
use fauna_core::encoding::{canonical_decode, canonical_encode};
use fauna_core::seen_set::SeenScopeSet;
use fauna_protocol::RpcRequester;
use fauna_protocol::merge_policy::KIND_SEEN_SET;
use fauna_protocol::scope::ContentScope;

use crate::account_state_plane::{AccountStatePlane, ItemId};
use crate::scope_set::OWN_ACTOR_KINDS;

/// One app report: *this record's body was handed to a visible view.*
///
/// Constructing one is an assertion about the display, not about a list — see
/// the module docs' division. The intake decides everything else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    /// The scope whose feed carried the record (the `conv` channel scope for a
    /// message; a followed content scope as those land).
    pub scope: ContentScope,
    /// The record whose body was displayed — its canonical block CID, the
    /// shipped record identity.
    pub record: ContentHash,
}

impl Observation {
    /// Build one from the **string** plane identity an app's model carries —
    /// `fauna_conversations::message::PlaneRef`'s `(scope, record_digest)` pair
    /// — so no app hand-parses hex or hand-builds a scope string.
    ///
    /// The seam exists because the model crates that carry the identity are
    /// deliberately wire-type-free (`fauna-conversations` takes no
    /// `fauna-protocol` dependency), so the typed values can only be recovered
    /// here. It is also where the two crates' spelling of a content scope is
    /// actually checked against each other: a drift on either side yields
    /// `None` at this call rather than an observation that resolves nothing.
    ///
    /// `None` for a malformed scope string or a digest that is not 32 hex
    /// bytes. A caller reporting a rendered body treats that as "nothing to
    /// report" — never an error to surface: the set is grow-only, so a dropped
    /// report costs at most one re-render.
    pub fn parse(scope: &str, record_digest_hex: &str) -> Option<Self> {
        let scope = match scope.parse::<fauna_protocol::scope::Scope>().ok()? {
            fauna_protocol::scope::Scope::Content(c) => c,
            // The account-state scope is class-2 machinery, a folder scope
            // names a shared file set, and a group scope names a T20 storage
            // group, and an `ext` scope a third party's own state rows —
            // none of them is browse content.
            // Left deliberately exhaustive (no wildcard): that is what turned
            // the `Group` variant's arrival into a compile error here instead
            // of a silently-dropped observation class.
            fauna_protocol::scope::Scope::AccountState
            | fauna_protocol::scope::Scope::Folder(_)
            | fauna_protocol::scope::Scope::Group(_)
            | fauna_protocol::scope::Scope::Ext(_) => return None,
        };
        let digest: [u8; 32] = hex::decode(record_digest_hex)
            .ok()?
            .as_slice()
            .try_into()
            .ok()?;
        Some(Self {
            scope,
            record: ContentHash::from_digest_dag_cbor(digest),
        })
    }
}

/// What one observation did, for the caller's telemetry and for tests that
/// need to distinguish "ignored" from "recorded".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationOutcome {
    /// Membership grew and the entry was published.
    Recorded,
    /// The coordinate was already in the set — nothing published (the
    /// echo-stop: re-rendering a message every frame writes nothing).
    AlreadyIn,
    /// The local journal holds no row for this record in this scope yet.
    /// Dropped; a later render re-records it.
    Unresolved,
    /// The scope is an own-actor scope, whose items are creation- or
    /// delivery-class and already covered by the auto-in-set producer's
    /// watermark. T1 governs browse content only, so this is not an error —
    /// it is the classification doing its job.
    NotBrowseContent,
}

/// Whether `scope` carries browse content — the scopes T1 governs.
///
/// Browse content is everything that is *not* an own-actor scope: an item in a
/// member or subscribed scope (`conv` today; followed content scopes as they
/// land) is admitted by observation, because nobody on this account created or
/// was delivered it. Deriving the predicate by negation rather than an
/// allow-list is deliberate: a new member/subscribed kind is then governed by
/// T1 the day it starts walking, instead of silently falling outside every
/// producer until someone remembers to extend a list.
pub fn is_browse_scope(scope: &ContentScope) -> bool {
    !OWN_ACTOR_KINDS.contains(&scope.kind())
}

/// Record one body-rendered observation into the account's seen-set.
///
/// Idempotent and cheap on the common path: a re-render of an already-recorded
/// message reads the merged entry, finds the coordinate present, and publishes
/// nothing. [`ObservationOutcome::Recorded`] means the entry is durable
/// locally; its publish is the caller's (the account runtime arms its publish
/// step on it).
///
/// **Call this serially per scope — it is a read-modify-write, not a CAS.** It
/// reads the scope's merged entry, inserts the ref, and puts the value it
/// computed verbatim, so two overlapping calls for one scope both read the same
/// base and the later put drops the earlier ref from the *local* entry. The
/// production caller that supplies the serialization is the account runtime:
/// every observation crosses `AccountStoreHandle::record_observation` into
/// `Cmd::RecordObservation`, and the store thread awaits each call whole before
/// taking the next command. Shells are free to report concurrently on top of
/// that — tui spawns a task per frame — precisely because the handle funnels
/// them. A new caller reaching this function without crossing that handle
/// reintroduces the race — which is why it is **`pub(crate)`**: the handle is
/// the only door, structurally, so the six queued app reporting
/// legs cannot reach the unserialized form even by
/// accident. Ruled by the security review, on an escalation that
/// offered two options — build per-scope serialization now, or keep the doc
/// comment plus the pin. Neither:
/// a serialization mechanism ahead of the consumer that needs it is the pattern
/// this project avoids, and a requirement whose only witness is prose read by
/// another crate is the exact shape that gets lost (
/// a grant window documented once and checked at one of four decode sites).
/// Closing the door costs nothing and builds no mechanism.
///
/// Pinned by `account_runtime::tests::overlapping_frame_reports_for_one_scope_both_survive_locally`,
/// which asserts on the local entry: the fleet merge unions the published rows
/// and would hide the loss.
/// The `pub(crate)` above has a consequence the ruling did not have to state
/// but the compiler does: the only door — `AccountStoreHandle` in
/// `account_runtime` — is behind `feature = "account-runtime"`, while this
/// module is not. So in a build without that feature the function is
/// unreachable **by construction**, which is the ruling working, not a defect;
/// `-D dead-code` cannot tell the two apart. Narrow, and tied to the feature by
/// name, so re-widening the visibility or moving the door makes this
/// attribute's condition false rather than leaving a blanket allow behind.
#[cfg_attr(not(feature = "account-driver"), allow(dead_code))]
pub(crate) async fn record_observation<B: StoreBackend, R: RpcRequester>(
    store: &AccountStore<B>,
    plane: &AccountStatePlane<'_, B, R>,
    observation: &Observation,
) -> anyhow::Result<ObservationOutcome> {
    if !is_browse_scope(&observation.scope) {
        return Ok(ObservationOutcome::NotBrowseContent);
    }
    let scope_text = observation.scope.to_string();

    // Coordinate resolution: the scope-feed coordinate this replica journaled
    // for the record. Absent = the walk has not carried it here yet.
    let Some((writer, seq)) = store
        .coordinate_of_item(&scope_text, &ItemRef::Cid(observation.record))
        .await?
    else {
        return Ok(ObservationOutcome::Unresolved);
    };

    // Dedup against the merged entry — the same read-modify-write the
    // auto-in-set producer uses, and the same never-clobber rule: an entry
    // this binary cannot read is a loud failure, never overwritten.
    let mut set = match store.state(KIND_SEEN_SET, &scope_text).await? {
        Some(entry) => canonical_decode::<SeenScopeSet>(&entry.value)
            .map_err(|e| anyhow::anyhow!("stored entry does not decode: {e}"))?,
        None => SeenScopeSet::new(),
    };
    if !set.insert_ref(writer.0, seq) {
        return Ok(ObservationOutcome::AlreadyIn);
    }

    // The local write only: the account runtime's publish step ships it
    // (`Cmd::is_local`, the observation verdict) — nothing between the read
    // above and this write yields, so no walk page merges between them.
    let value = canonical_encode(&set)?;
    plane
        .put_local(
            &ItemId {
                kind: KIND_SEEN_SET.to_string(),
                key: scope_text,
            },
            value,
            // CrdtPerField carries no LwwStamp — the grow-only join orders.
            None,
        )
        .await?;
    Ok(ObservationOutcome::Recorded)
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::SigningKey;
    use fauna_account_store::sqlite::SqliteBackend;
    use fauna_account_store::types::{JournalOp, JournalRow, WriterId};
    use fauna_core::crypto::{AccountStateKeySchedule, BackupKey};
    use fauna_core::identity::ActorKeypair;
    use fauna_protocol::account_state::ACCOUNT_STATE_SCOPE;

    use super::*;
    use crate::scope_set::CONV_KIND;

    /// The intake never publishes over RPC in these tests (pull-only plane), so
    /// any request reaching the wire is itself the failure.
    struct NoNest;

    impl RpcRequester for NoNest {
        type Error = anyhow::Error;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            _payload: Req,
        ) -> anyhow::Result<Reply>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            anyhow::bail!("pull-only put reached the wire ({kind}) — it must be local-only")
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        store: AccountStore<SqliteBackend>,
        schedule: AccountStateKeySchedule,
        writer_key: SigningKey,
        trust: crate::generation_tip::GenerationTrust,
        actor: [u8; 32],
    }

    async fn fixture() -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let kp = ActorKeypair::generate();
        let writer_key = SigningKey::from_bytes(&[7u8; 32]);
        let writer = WriterId(writer_key.verifying_key().to_bytes());
        let store = AccountStore::open(
            SqliteBackend::open(dir.path()).unwrap(),
            &kp.actor_id_hex(),
            writer,
        )
        .await
        .unwrap();
        Fixture {
            _dir: dir,
            store,
            schedule: AccountStateKeySchedule::derive(&BackupKey::derive(kp.secret_bytes())),
            writer_key,
            trust: crate::generation_tip::GenerationTrust {
                root: kp.actor_id(),
                prior: Vec::new(),
                trusted_holders: Default::default(),
            },
            actor: fauna_core::hex32::decode(&kp.actor_id_hex()).unwrap(),
        }
    }

    impl Fixture {
        fn plane(&self) -> AccountStatePlane<'_, SqliteBackend, NoNest> {
            AccountStatePlane::new_pull_only(
                &self.store,
                &NoNest,
                &self.schedule,
                &self.writer_key,
                &self.trust,
                ACCOUNT_STATE_SCOPE,
            )
            .expect("plane")
        }

        /// Journal one nest-sequenced row carrying `record` into `scope`, as a
        /// content walk would.
        async fn journal(&self, scope: &str, seq: u64, record: ContentHash) {
            self.store
                .ingest_row(&JournalRow {
                    writer: WriterId::NEST_SEQUENCER,
                    seq,
                    scope: scope.to_string(),
                    op: JournalOp::RecordAdded,
                    item: ItemRef::Cid(record),
                })
                .await
                .expect("ingest");
        }

        async fn stored_set(&self, scope: &str) -> Option<SeenScopeSet> {
            self.store
                .state(KIND_SEEN_SET, scope)
                .await
                .expect("state")
                .map(|e| canonical_decode(&e.value).expect("decode"))
        }
    }

    fn record(n: u8) -> ContentHash {
        ContentHash::from_digest_dag_cbor([n; 32])
    }

    fn conv_scope(channel: [u8; 32]) -> ContentScope {
        ContentScope::new(CONV_KIND, channel).expect("scope")
    }

    /// The row's headline property: a body rendered in a member scope lands
    /// **that record's** coordinate — itemized, and only that one.
    #[tokio::test]
    async fn a_rendered_body_lands_its_own_coordinate_itemized() {
        let fx = fixture().await;
        let scope = conv_scope([9u8; 32]);
        let text = scope.to_string();
        // Three messages walked into the replica; the user reads the middle one.
        fx.journal(&text, 1, record(1)).await;
        fx.journal(&text, 2, record(2)).await;
        fx.journal(&text, 3, record(3)).await;

        let outcome = record_observation(
            &fx.store,
            &fx.plane(),
            &Observation {
                scope: scope.clone(),
                record: record(2),
            },
        )
        .await
        .expect("intake");
        assert_eq!(outcome, ObservationOutcome::Recorded);

        let set = fx.stored_set(&text).await.expect("entry published");
        assert_eq!(
            set.refs,
            vec![fauna_core::seen_set::SeenRef {
                writer: WriterId::NEST_SEQUENCER.0,
                seq: 2,
            }],
            "only the rendered record is in-set"
        );
        assert!(
            set.watermarks.is_empty(),
            "browse content is itemized, never watermarked — a watermark would \
             assert the whole prefix at-or-below was observed, which reading one \
             message does not earn"
        );
        assert!(
            !set.contains(&WriterId::NEST_SEQUENCER.0, 1),
            "the message merely above it in the list is NOT in-set"
        );
        assert!(
            !set.contains(&WriterId::NEST_SEQUENCER.0, 3),
            "the message merely below it in the list is NOT in-set"
        );
    }

    /// The echo-stop: re-rendering an already-recorded body publishes nothing,
    /// so a scrolled-back-to message does not rewrite the entry every frame.
    #[tokio::test]
    async fn re_rendering_an_in_set_body_publishes_nothing() {
        let fx = fixture().await;
        let scope = conv_scope([9u8; 32]);
        let text = scope.to_string();
        fx.journal(&text, 1, record(1)).await;
        let obs = Observation {
            scope,
            record: record(1),
        };

        assert_eq!(
            record_observation(&fx.store, &fx.plane(), &obs)
                .await
                .expect("first"),
            ObservationOutcome::Recorded
        );
        let first = fx.stored_set(&text).await.expect("entry");
        assert_eq!(
            record_observation(&fx.store, &fx.plane(), &obs)
                .await
                .expect("second"),
            ObservationOutcome::AlreadyIn
        );
        assert_eq!(
            fx.stored_set(&text).await.expect("entry"),
            first,
            "the entry is byte-identical — nothing republished"
        );
    }

    /// An observation the local journal cannot place is dropped, not queued:
    /// no entry, no pending state, and a later render re-records it once the
    /// walk has carried the row here.
    #[tokio::test]
    async fn an_unresolvable_observation_is_dropped_and_re_recordable() {
        let fx = fixture().await;
        let scope = conv_scope([9u8; 32]);
        let text = scope.to_string();
        let obs = Observation {
            scope,
            record: record(4),
        };

        assert_eq!(
            record_observation(&fx.store, &fx.plane(), &obs)
                .await
                .expect("intake"),
            ObservationOutcome::Unresolved
        );
        assert!(
            fx.stored_set(&text).await.is_none(),
            "nothing written for a coordinate that does not resolve"
        );

        // The walk catches up; the next render succeeds with no help from any
        // remembered state.
        fx.journal(&text, 7, record(4)).await;
        assert_eq!(
            record_observation(&fx.store, &fx.plane(), &obs)
                .await
                .expect("intake"),
            ObservationOutcome::Recorded
        );
        assert!(
            fx.stored_set(&text)
                .await
                .expect("entry")
                .contains(&WriterId::NEST_SEQUENCER.0, 7)
        );
    }

    /// The string seam an app reports through: the model crates carry a scope
    /// string + a digest hex (they are wire-type-free by design), and this is
    /// the only place those become typed values.
    #[test]
    fn parse_lifts_the_string_plane_identity_into_an_observation() {
        let channel = [0x4C; 32];
        let scope = ContentScope::new("conv", channel).expect("scope");
        let digest = [0x5D; 32];
        let parsed = Observation::parse(&scope.to_string(), &hex::encode(digest))
            .expect("a well-formed pair parses");
        assert_eq!(
            parsed,
            Observation {
                scope,
                record: ContentHash::from_digest_dag_cbor(digest),
            },
        );
    }

    /// Every malformed half yields `None` rather than an observation that would
    /// resolve nothing — including the account-state scope, which is class-2
    /// machinery and never browse content.
    #[test]
    fn parse_refuses_a_malformed_or_non_content_pair() {
        let good_scope = ContentScope::new("conv", [0x4C; 32])
            .expect("scope")
            .to_string();
        let good_digest = hex::encode([0x5D; 32]);
        assert!(
            Observation::parse("", &good_digest).is_none(),
            "empty scope"
        );
        assert!(
            Observation::parse("content:conv:nothex", &good_digest).is_none(),
            "malformed scope id",
        );
        assert!(
            Observation::parse(ACCOUNT_STATE_SCOPE, &good_digest).is_none(),
            "the account-state scope is not browse content",
        );
        assert!(
            Observation::parse(&good_scope, "").is_none(),
            "empty digest",
        );
        assert!(
            Observation::parse(&good_scope, &hex::encode([0u8; 16])).is_none(),
            "a 16-byte digest is not a record id",
        );
    }

    /// Classification: an own-actor scope is the auto-in-set producer's, and
    /// T1 declines it rather than itemizing beside a watermark.
    #[tokio::test]
    async fn an_own_actor_scope_is_not_browse_content() {
        let fx = fixture().await;
        let scope = ContentScope::new("post", fx.actor).expect("scope");
        let text = scope.to_string();
        fx.journal(&text, 1, record(1)).await;

        assert_eq!(
            record_observation(
                &fx.store,
                &fx.plane(),
                &Observation {
                    scope,
                    record: record(1),
                },
            )
            .await
            .expect("intake"),
            ObservationOutcome::NotBrowseContent
        );
        assert!(fx.stored_set(&text).await.is_none(), "nothing written");
        assert!(
            !is_browse_scope(&ContentScope::new("post", fx.actor).unwrap()),
            "own-actor kinds are not browse content"
        );
        assert!(
            is_browse_scope(&conv_scope([1u8; 32])),
            "conv is browse content"
        );
    }

    /// The coordinate is the row that *introduced* the record, so a later op on
    /// the same item cannot move an already-published membership.
    #[tokio::test]
    async fn the_coordinate_is_the_introducing_row() {
        let fx = fixture().await;
        let scope = conv_scope([9u8; 32]);
        let text = scope.to_string();
        fx.journal(&text, 3, record(5)).await;
        fx.journal(&text, 8, record(5)).await; // a later op on the same item

        record_observation(
            &fx.store,
            &fx.plane(),
            &Observation {
                scope,
                record: record(5),
            },
        )
        .await
        .expect("intake");

        let set = fx.stored_set(&text).await.expect("entry");
        assert_eq!(
            set.refs,
            vec![fauna_core::seen_set::SeenRef {
                writer: WriterId::NEST_SEQUENCER.0,
                seq: 3,
            }]
        );
    }

    /// Never clobber an entry this binary cannot read — the same rule the
    /// auto-in-set producer holds, restated here because this producer is the
    /// one a *render* can drive at any moment.
    #[tokio::test]
    async fn an_undecodable_entry_is_reported_and_left_intact() {
        use fauna_account_store::types::StateEntry;
        let fx = fixture().await;
        let scope = conv_scope([9u8; 32]);
        let text = scope.to_string();
        fx.journal(&text, 1, record(1)).await;
        let garbage = b"not dag-cbor".to_vec();
        fx.store
            .put_state(StateEntry {
                kind: KIND_SEEN_SET.to_string(),
                key: text.clone(),
                scope: ACCOUNT_STATE_SCOPE.to_string(),
                value: garbage.clone(),
                merge_meta: None,
                entry_version: 0,
                tombstone: false,
            })
            .await
            .expect("poison");

        let err = record_observation(
            &fx.store,
            &fx.plane(),
            &Observation {
                scope,
                record: record(1),
            },
        )
        .await
        .expect_err("an unreadable entry is loud");
        assert!(
            format!("{err:#}").contains("does not decode"),
            "the failure says what happened: {err:#}"
        );
        assert_eq!(
            fx.store
                .state(KIND_SEEN_SET, &text)
                .await
                .expect("state")
                .expect("entry")
                .value,
            garbage,
            "never clobbered"
        );
    }
}
