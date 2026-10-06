//! The client-side index builder: receive hook → sealed segment → `__index`
//! publish.
//!
//! This is the first real writer the index format has ever had
//! (`content-index.md` § Status / plan state; the only prior writer was the
//! nest-side one deleted 2026-07-13 for writing *unsealed* segments).
//! Everything it writes uses the S0-ratified per-kind key split, so the later
//! MDA leg (rollout S5) reads and extends the very same slices.
//!
//! One [`IndexBuilder`] serves **both** key classes, selected by the
//! [`fauna_index::ClassKey`] it is constructed with: the MSEK-derived
//! mail/calendar class (rollout S3) and the identity-seed-derived master class
//! (rollout S4). Named `mail_sink` until 2026-08-05, when the master class
//! arrived and the name stopped being true — note `apps/fauna-linux` has its
//! own, unrelated `mail_sink.rs` (the inbound poll), which is why the doc
//! comments elsewhere say *"linux's `mail_sink.rs`"*.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::compaction::{COMPACTION_LIVE_SEGMENT_THRESHOLD, FoldCandidate, plan_fold};
use fauna_conversations::index_sink::{
    IndexableDraft, IndexableKind, IndexableMessage, MessageIndexObserver,
};
use fauna_index::{
    ClassKey, ContentId, ContentKind, DocIdentity, FieldKind, Index, IndexError, IndexManifest,
    IndexMasterKey, IndexSegmentKey, IndexedDoc, IndexedField, KindClass,
    TOKENIZER_PIPELINE_VERSION, segment_path,
};

/// One sealed blob the `__index` rail currently holds, as the fold planner
/// sees it: enough to choose inputs by size and then fetch them, without a
/// second listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RailEntry {
    /// The `__index`-relative virtual path, as published.
    pub path: String,
    /// Hex blake3 hash of the sealed blob — the handle [`SegmentRail::fetch_blob`] takes.
    pub blob_hash: String,
    /// Size of the sealed blob. The fold planner budgets against this so a
    /// merged segment cannot outgrow the one-blob ceiling.
    pub size_bytes: u64,
}

/// The `__index` rail, in both directions. Kept as a seam so this crate carries
/// no sync dependency: the production implementation rides the fauna-sync file
/// plane into the `__index` reserved folder (a per-actor reserved set —
/// `content-index.md` § Ingest triggers, v1), and tests
/// substitute a recorder.
///
/// **It reads as well as writes because compaction folds.** An append-only
/// publisher was enough while a flush only ever added a segment, but
/// fold-at-flush (§ Where the index is built) merges the staged batch with live
/// segments the builder published in an *earlier* session and no longer holds —
/// so the builder has to fetch them back. The read half mirrors the rail's real
/// shape (one `list`, then a fetch per blob hash), which is what keeps a fold
/// to the same round-trip count as opening the index.
///
/// The nest on the other side can never read any of it: what crosses this seam
/// is already AEAD-sealed under a key no nest holds.
#[async_trait::async_trait]
pub trait SegmentRail: Send + Sync {
    /// Everything the rail currently lists for this actor. One call, so a fold
    /// can size its candidates and fetch them from the same view.
    async fn list_entries(&self) -> Result<Vec<RailEntry>, IndexBuildError>;

    /// Sealed bytes for a blob hash from [`Self::list_entries`].
    async fn fetch_blob(&self, blob_hash: &str) -> Result<Vec<u8>, IndexBuildError>;

    /// Publish `bytes` at the `__index`-relative virtual path `path`
    /// (`fauna_index::segment_path` / `mailcal_manifest_path` produce it).
    /// Overwrites any prior blob at that path — segments are written once and
    /// never mutated, but the manifest is rewritten on every append.
    ///
    /// Async because the production implementation rides the fauna-sync byte
    /// plane (upload the chunked blob, then record the path through the sync
    /// journal so the user's other locations replicate it). Only
    /// [`IndexBuilder::flush`] awaits it — never the receive path, whose
    /// non-blocking contract is what splits staging from flushing.
    async fn publish(&self, path: &str, bytes: &[u8]) -> Result<(), IndexBuildError>;
}

/// Hard ceiling on one sealed segment's bytes.
///
/// **Not a configuration surface** — no user or admin would ever choose this
/// (product invariants § the only configuration surface is the apps), so it is
/// a Rust constant. It exists because the `__index` rail carries *one whole
/// blob per path* with no chunk-manifest form (`content-index.md` § Ingest
/// triggers, v1), so a segment that outgrew the blob route would have nowhere
/// to go: the builder bounds its own size rather than spilling to
/// `/api/v1/chunks`.
///
/// 8 MiB sits under the nest's 10 MiB `BLOB_BODY_LIMIT` with room for request
/// framing. Row 5's compactor must respect the same ceiling when it folds
/// segments together.
pub const MAX_SEGMENT_BYTES: usize = 8 * 1024 * 1024;

/// How many staged docs one flush may seal into a single segment.
///
/// The *primary* bound — cheap, and it keeps a large backfill producing a
/// segment chain instead of one enormous blob. [`MAX_SEGMENT_BYTES`] is the
/// backstop for the case this cap cannot predict (a few very large messages),
/// which [`IndexBuilder::flush`] handles by halving the batch.
pub const MAX_DOCS_PER_SEGMENT: usize = 2_000;

/// One sealed blob and the virtual path it was published at — what
/// [`IndexBuilder::flush`] reports for callers that want to log or assert
/// on a build without re-reading the folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedSegment {
    /// The kind this segment holds. A flush publishes at most one segment per
    /// kind, so this is also what distinguishes the entries of one flush's
    /// report from each other.
    pub kind: ContentKind,
    pub path: String,
    pub byte_len: usize,
    pub doc_count: usize,
}

/// One sealed segment's bytes and how many docs went into it.
struct SealedBatch {
    bytes: Vec<u8>,
    doc_count: usize,
}

/// What [`IndexBuilder::seal_bounded`] got out of a staged batch: at most
/// one segment that fits the ceiling, plus the docs that did not fit and must
/// be re-staged for the next flush.
struct BoundedSeal {
    segment: Option<SealedBatch>,
    deferred: Vec<IndexedDoc>,
    /// The docs that went *into* `segment`, handed back so a failed flush can
    /// re-stage them.
    ///
    /// Sealing consumes the batch, and `take_batch` has already drained
    /// `pending` by the time anything is sealed — so without this the docs of a
    /// flush that failed to publish were simply gone for the rest of the
    /// session, while their ids stayed in the re-index guard. The "retrying on
    /// the next trigger" the failure log promises then retried *nothing* for
    /// them: an append kind's docs stayed unsearchable until relaunch, and a
    /// snapshot kind's corpus until the producer happened to offer it again.
    sealed_docs: Vec<IndexedDoc>,
}

/// A fold this flush has decided to take: the merged segment's sealed bytes
/// and the live ids it supersedes, ready for the manifest write to append the
/// one and tombstone the others.
struct PreparedFold {
    /// Ascending live segment ids folded into `sealed`. Tombstoned — never
    /// deleted (`content-index.md` § Where the index is built).
    inputs: Vec<u32>,
    sealed: Vec<u8>,
    /// Live docs in the merged segment, i.e. the staged batch plus every folded
    /// input.
    doc_count: usize,
}

/// One kind's sealed segment, with the id the manifest write assigned it.
struct PlannedSegment {
    kind: ContentKind,
    path: String,
    bytes: Vec<u8>,
    doc_count: usize,
}

/// Everything one flush has to publish, computed under the manifest lock and
/// handed out for the async half to write.
///
/// `segments` holds at most one entry per kind and is empty on a cursor-only
/// flush — but `manifest_bytes` is always present, because the whole point of
/// planning under one lock is that the several kinds' segment ids and the single
/// manifest that lists them can never disagree.
struct FlushPlan {
    segments: Vec<PlannedSegment>,
    manifest_bytes: Vec<u8>,
    /// The manifest this flush *would* leave behind — planned on a **clone**, so
    /// a publish failure leaves the builder's live manifest untouched.
    ///
    /// [`IndexBuilder::flush`] commits it back only after every segment **and**
    /// the manifest itself have reached the rail. Planning in place was a real
    /// defect: the id assignments and tombstones landed before any publish and
    /// survived its failure, so the next successful flush published them
    /// durably — a manifest naming segments the rail never received (an append
    /// kind), or, worse, a snapshot kind's only real corpus tombstoned in
    /// favour of a blob that does not exist. That made the **error** path
    /// weaker than the crash path, which this file's publish order is otherwise
    /// careful to keep safe.
    manifest: IndexManifest,
}

#[derive(Debug, thiserror::Error)]
pub enum IndexBuildError {
    #[error("index format error: {0}")]
    Index(#[from] IndexError),
    /// The publisher could not place a blob. The builder treats this as
    /// retryable: nothing is lost, because the docs are re-derivable from the
    /// content and the advisory cursor was not advanced.
    #[error("publishing `{path}` failed: {reason}")]
    Publish { path: String, reason: String },
}

impl IndexBuildError {
    /// Whether retrying this can ever succeed on its own.
    ///
    /// The arms that resume a builder retry forever by design — a nest
    /// unreachable at login must not cost the user their receive loop — so the
    /// one thing they owe the user is not *promising* recovery that cannot
    /// come. A rail failure heals when the nest comes back; a blob this build
    /// cannot decode heals only when something outside the index changes (the
    /// app is updated, a rewrap pass runs).
    ///
    /// [`IndexError::Crypto`] is deliberately **not** here. On the master class
    /// it no longer reaches a caller at all (an unopenable blob takes the
    /// rebuild arm — `rail_publisher::UnopenableDisposition`), and on the
    /// mail/calendar class it is genuinely recoverable by waiting: the missing
    /// grace generation arrives with the next MLS snapshot sync.
    pub fn is_permanent(&self) -> bool {
        matches!(
            self,
            IndexBuildError::Index(
                IndexError::Incompatible { .. }
                    | IndexError::SchemaMismatch(_)
                    | IndexError::WrongKindClass { .. }
            )
        )
    }
}

/// Builds and publishes **one kind's** slice of a user's content index.
///
/// Lifecycle: app glue constructs one per logged-in actor, registers it as the
/// conversations manager's index observer, and drives [`Self::flush`] from the
/// debounce trigger (the `fauna.mail.received` push plus a periodic timer —
/// the `segment_backup.rs` precedent). Ingest is split from flush on purpose:
/// the observer seam's contract is that it **must not block** the receive path
/// (`fauna_conversations::index_sink`), so observing only buffers a tokenizable
/// doc, and all the CPU (tokenize, seal) and I/O (publish) happen at flush.
///
/// **One builder per (class, kind), and the class is carried by the key.** The
/// two classes never share a builder because they never share a key: the
/// mail/calendar class seals under the MSEK-derived segment key an MDA bridge
/// can reach, the master class under the client-only index master key
/// (`content-index.md` § Don't do these — *never wrap a master-key kind under a
/// key a MUA credential reaches*; `key-material-hierarchy.md` rule #7).
/// [`ClassKey`] is what makes that split one value instead of a parallel set of
/// `*_mailcal` call sites.
///
/// ⚠ **A class has exactly ONE manifest file, which is why this builder holds a
/// kind SET rather than one kind.** `manifest.idx` covers every master kind
/// (conversation, post, contact, draft, file, media) and each builder rewrites
/// its manifest wholesale on flush — so two master-class builders each holding
/// their own [`IndexManifest`] would clobber each other's segment chains,
/// silently losing published segments. One builder per **class** makes that
/// unrepresentable: the flush batch is grouped by [`IndexedDoc::kind`], one
/// segment per kind, and the single manifest that lists them all is written once
/// under one lock (`content-index.md` § Encryption posture — *a kind never
/// appears in the wrong-class manifest*).
///
/// Per-kind state stays per kind even so: each kind keeps its own segment id
/// chain (the manifest already keys those), its own advisory ingest cursor, and
/// its own catch-up window — the last because a boundary signal says nothing
/// about a sibling kind's backlog (`content-index.md` § Ingest triggers, v1 —
/// *The boundary signal is per kind*).
pub struct IndexBuilder {
    /// This builder's class wrap key — and, through [`ClassKey::class`], the
    /// class it may append to. Segments and the class manifest seal under this
    /// and nothing else.
    key: ClassKey,
    /// The [`ContentKind`]s this builder stages — every kind of its class that
    /// this seat builds. Invariant, checked at construction: every kind's
    /// `class()` equals `key.class()`. The kind is *also* carried per-doc on
    /// [`IndexedDoc`], and that per-doc kind — not this set — is what the
    /// manifest operations (append, tombstone, cursor, segment path) key on.
    ///
    /// Ordered, so a flush that publishes several kinds' segments does so in a
    /// deterministic order: a test can assert on the published sequence, and two
    /// seats racing on the same manifest produce comparable histories.
    kinds: BTreeSet<ContentKind>,
    /// This class's manifest. Rewritten wholesale on every flush.
    manifest: Mutex<IndexManifest>,
    /// Docs observed since the last flush.
    pending: Mutex<Vec<IndexedDoc>>,
    publisher: Arc<dyn SegmentRail>,
    /// Content ids this builder must not index again: seeded at resume from the
    /// segments already published (`resume_mail_builder`), then grown by every
    /// doc [`Self::stage`] accepts.
    ///
    /// **This is the re-index guard, and it is not an optimisation.** The client
    /// receive path has no restart-durable cursor — `fauna_conversations`'s mail
    /// cursors are per-session and start at UID 0 — so every launch re-pages the
    /// whole mailbox and the observer seam fires for all of it. Without this
    /// set, each launch would seal and publish the entire mailbox again as a
    /// fresh segment chain: unbounded growth on the user's at-rest store, with
    /// the ruled compactor not yet built (`content-index.md` § Ingest triggers,
    /// v1 — the 2026-08-03 correction, which also records why an advisory UID
    /// cursor cannot do this job: one `u64` per `ContentKind`, two independent
    /// mailbox id spaces).
    ///
    /// Safe for **immutable** kinds only, because it can never re-index a
    /// superseded version. Mail qualifies (the content id is the producer-owned
    /// RFC `Message-ID`), and **Conversation qualifies too** — message content
    /// is immutable post-ingest, the store dedups by message id, and no edit
    /// path exists, which is exactly why the 2026-08-04 ruling let the guard
    /// transfer to it (`content-index.md` § Ingest triggers, v1 → *The
    /// Conversation kind's catch-up*). It stays **wrong for drafts**: that arm
    /// owes its own dedup answer rather than copying this one.
    ///
    /// **The value is the doc's nest segment-record id, and that is what makes
    /// the guard safe for mail rather than merely cheap** (ruled 2026-09-16,
    /// `content-index-ingest.md` § Ingest triggers, v1 → *A mail content id is
    /// a stable identity, not immutable content*). Skip-if-present rested on
    /// "a mail content id is the immutable RFC `Message-ID`", and the
    /// Message-ID collision rule (`../../docs/goal/ui/conversations.md`
    /// § Receiving into the conversations view) falsified the content half of
    /// that: when a `Sent` copy displaces a squatting `INBOX` copy, the content
    /// under a Message-ID this device already indexed is *replaced*. Keyed on
    /// identity alone the guard would drop the genuine copy's observe for ever
    /// — the account's own message never tokenized, the squat's words still
    /// matching. So a doc is "already indexed" only when it arrives under the
    /// **same nest record** as the one indexed; a different record id means the
    /// nest filed a different record under this id, which is precisely a
    /// supersession.
    ///
    /// **Why the nest's record id and not a displacement event on the seam.**
    /// The squat re-pages from `INBOX` every launch and the thread store starts
    /// empty every launch, so the displacement *re-fires every launch*: an
    /// event-driven rebuild would republish a segment blob per launch for as
    /// long as the squat sits in the mailbox. Comparing record ids is instead
    /// idempotent by construction — after the rewrite the live doc carries the
    /// `Sent` record's id, so the next launch matches and does nothing. It is
    /// also the one discriminator here that a forger does not control: the nest
    /// assigns record ids, where the Message-ID and every header are the
    /// attacker's to choose.
    ///
    /// `None` on both sides matches, so a rail that carries no nest record
    /// (MLS conversations, the drafts corpus, attach-time walks) keeps exactly
    /// the pre-2026-09-16 behaviour.
    ///
    /// **Keyed by content id alone, not by `(kind, content id)`, deliberately**
    /// — even now that one builder spans several kinds of its class. The key
    /// matches [`fauna_index::Index::add_doc`]'s own upsert key, which is the
    /// content id by itself; a guard keyed more finely than the index it guards
    /// would let a doc past that the index then silently overwrites, which is a
    /// worse failure than the one it would prevent. Two kinds colliding on one
    /// content id costs a skipped doc, and the id vocabularies (RFC
    /// `Message-ID`, 32-byte content hashes) do not overlap in practice.
    indexed: Mutex<HashMap<Vec<u8>, Option<Vec<u8>>>>,
    /// Content ids whose staged doc **replaces** one an already-published
    /// segment holds — the supersessions this flush owes a segment rewrite.
    ///
    /// Populated by [`Self::stage_doc`] when the re-index guard above finds the
    /// id present under a *different* nest record id. Drained by the flush,
    /// which rewrites the live segments holding those ids
    /// ([`fauna_index::Index::rebuild_with`]) instead of appending the doc, so
    /// the stale copy stops matching rather than merely being outnumbered.
    ///
    /// Empty in every ordinary flush: nothing pays for this but the flush that
    /// actually carries a displacement.
    superseding: Mutex<BTreeSet<Vec<u8>>>,
    /// The kinds whose launch catch-up walk is still in front of us — every kind
    /// this builder stages whose backlog re-presents ([`has_catch_up_window`]),
    /// until [`Self::end_catch_up`] retires it, which the receive loop drives
    /// per kind through `MessageIndexObserver::observe_catch_up_complete`.
    ///
    /// **Per kind rather than one flag**, because a boundary signal describes
    /// exactly one kind's backlog: mail's sweep closing a sibling's window
    /// mid-walk would reclassify that sibling's remaining backlog as trickle,
    /// which the lease never gates (`content-index.md` § Ingest triggers, v1 —
    /// *The boundary signal is per kind*).
    ///
    /// Only meaningful together with [`Self::lease_gate`]: it is the half of the
    /// stand-down rule that says *which* work a closed gate may withhold.
    catching_up: Mutex<BTreeSet<ContentKind>>,
    /// Snapshot kinds whose corpus has been staged since the last flush.
    ///
    /// Separate from [`Self::pending`] because the *absence* of docs is itself
    /// the event when a user discards their last draft: the batch is empty, and
    /// only this set can tell "nothing was staged" (leave the kind alone) from
    /// "an empty corpus was staged" (retire everything still live).
    snapshot_staged: Mutex<BTreeSet<ContentKind>>,
    /// Serializes [`Self::flush`] against itself.
    ///
    /// Load-bearing since the flush became **plan-on-a-clone, commit-on-success**
    /// (see [`FlushPlan::manifest`]): the commit writes a whole manifest back, so
    /// two flushes overlapping would let the later one's commit discard the
    /// earlier's segment ids and tombstones — a regression the previous
    /// mutate-in-place shape could not have. The production driver
    /// (`lifecycle.rs`) already awaits each flush before starting the next, so
    /// this contends only if a caller drives `flush` itself; making it structural
    /// rather than a documented caller convention is what keeps the commit safe.
    ///
    /// A `tokio` mutex rather than a `std` one because it is held across the
    /// publish `await`s.
    flushing: tokio::sync::Mutex<()>,
    /// The advisory `index` lease's gate — `true` ⇒ this seat holds the lease.
    ///
    /// **`None` means uncoordinated, and that is a supported production state,**
    /// not merely a test shape: a build that wires no lease (app glue that
    /// supplies no device id) behaves byte-for-byte as it did
    /// before the lease existed. That is the same unattached-⇒-uncoordinated
    /// contract the (deleted) upload coordinator's `with_lease_gate` had, and it is what makes
    /// the heartbeat additive across a mixed fleet (`content-index.md` § Where
    /// the index is built → *Compatibility is additive by construction*).
    ///
    /// **What a closed gate withholds is deliberately narrow: catch-up staging
    /// only.** See [`Self::stage`].
    lease_gate: Option<Arc<AtomicBool>>,
    /// Always [`MAX_SEGMENT_BYTES`] in production — a field only so tests can
    /// exercise the halving path without an 8 MiB fixture. There is no setter
    /// outside `cfg(test)`, so this is not a configuration surface.
    max_segment_bytes: usize,
    /// Segments queued for retirement at the next flush — below-current-format blobs the
    /// resume skipped ([`crate::rail_publisher`]'s open: stamped below
    /// `CURRENT_INDEX_FORMAT_VERSION`, they cannot be opened beside a current
    /// segment). Applied as ordinary manifest tombstones at the top of
    /// `plan_flush`, cleared only after that flush commits, so a failed
    /// publish retries them. Tombstone-only, like every retirement
    /// (`content-index.md` § Where the index is built → the 2026-08-10
    /// carrier ruling); the guard was never seeded from these segments, so
    /// the next walk re-stages their docs under the current format.
    stale_retire: Mutex<Vec<(ContentKind, u32)>>,
}

impl IndexBuilder {
    /// Start a **mail-kind** builder over a fresh mail/calendar manifest — the
    /// first-run path, before any segment exists for this actor.
    ///
    /// Takes the MSEK rather than a [`ClassKey`] because the mail class's key is
    /// derived, not held: `fauna_mls::wrapped_blob::derive_index_segment_key`
    /// is the one derivation, and keeping it here means no caller re-derives it.
    pub fn mail(msek: &[u8; 32], publisher: Arc<dyn SegmentRail>) -> Self {
        Self::with_manifest(
            Self::mail_key(msek),
            [ContentKind::Mail],
            IndexManifest::empty(KindClass::MailCal, TOKENIZER_PIPELINE_VERSION),
            publisher,
        )
    }

    /// The mail/calendar class key for `msek` — the single derivation site.
    pub fn mail_key(msek: &[u8; 32]) -> ClassKey {
        ClassKey::MailCal(IndexSegmentKey::from_bytes(
            *fauna_mls::wrapped_blob::derive_index_segment_key(msek),
        ))
    }

    /// Start a **master-class** builder for one kind over a fresh master
    /// manifest.
    ///
    /// The key is passed in rather than derived here: the master key comes from
    /// the identity seed via `fauna_core::crypto::derive_index_master_key`
    /// (`key-material-hierarchy.md` § Path A-sibling), and this crate does not
    /// hold seeds — `NestMailIndexLauncher` owns key custody and hands builders
    /// a key, never the material to make one (`content-index.md` § Where the
    /// index is built).
    pub fn master(
        key: IndexMasterKey,
        kinds: impl IntoIterator<Item = ContentKind>,
        publisher: Arc<dyn SegmentRail>,
    ) -> Self {
        Self::with_manifest(
            ClassKey::Master(key),
            kinds,
            IndexManifest::empty(KindClass::Master, TOKENIZER_PIPELINE_VERSION),
            publisher,
        )
    }

    /// Resume against an existing manifest — what the replica refresh hands
    /// back after opening this class's manifest off the synced `__index`
    /// replica, so a second run appends to the same segment chain instead of
    /// restarting the ids.
    ///
    /// # Panics
    ///
    /// If any of `kinds` does not belong to `key`'s class, or if `kinds` is
    /// empty. A wrong-class pairing is a programming error, not a runtime
    /// condition: it would seal a kind under a key whose class cannot hold it,
    /// and `append_segment`'s class guard would reject every flush thereafter.
    /// An empty set is a builder that can never stage anything, which is always
    /// a wiring mistake rather than a rollout state — an arm that should build
    /// nothing is simply not attached. Failing at construction names either
    /// mistake where it is made.
    pub fn with_manifest(
        key: ClassKey,
        kinds: impl IntoIterator<Item = ContentKind>,
        manifest: IndexManifest,
        publisher: Arc<dyn SegmentRail>,
    ) -> Self {
        let kinds: BTreeSet<ContentKind> = kinds.into_iter().collect();
        assert!(
            !kinds.is_empty(),
            "index builder: a builder must stage at least one kind",
        );
        for kind in &kinds {
            assert_eq!(
                kind.class(),
                key.class(),
                "index builder: {kind:?} belongs to {:?}, but the key seals {:?}",
                kind.class(),
                key.class(),
            );
        }
        Self {
            key,
            // Every kind whose backlog re-presents starts inside its catch-up
            // window: the launch walk that will close it has not run yet. A
            // kind whose backlog is offered exactly once has no window to be
            // inside (`has_catch_up_window`).
            catching_up: Mutex::new(
                kinds
                    .iter()
                    .copied()
                    .filter(|kind| has_catch_up_window(*kind))
                    .collect(),
            ),
            kinds,
            manifest: Mutex::new(manifest),
            pending: Mutex::new(Vec::new()),
            publisher,
            indexed: Mutex::new(HashMap::new()),
            superseding: Mutex::new(BTreeSet::new()),
            snapshot_staged: Mutex::new(BTreeSet::new()),
            lease_gate: None,
            max_segment_bytes: MAX_SEGMENT_BYTES,
            flushing: tokio::sync::Mutex::new(()),
            stale_retire: Mutex::new(Vec::new()),
        }
    }

    /// Queue segments for tombstone-only retirement at the next flush — the
    /// resume paths' below-current-format purge (`content-index.md` § Where the index is
    /// built → the 2026-08-10 carrier ruling). See [`Self::stale_retire`].
    pub fn retire_segments(&self, segments: Vec<(ContentKind, u32)>) {
        if segments.is_empty() {
            return;
        }
        self.stale_retire.lock().unwrap().extend(segments);
    }

    /// Put this builder's **catch-up** staging under the advisory `index` lease.
    ///
    /// `gate` is a live [`LeaseCoordinator`]'s gate — `true` while this seat
    /// holds the lease. Without this call the builder is uncoordinated, which is
    /// the pre-lease behaviour and stays correct (see [`Self::lease_gate`]).
    ///
    /// [`LeaseCoordinator`]: https://docs.rs/fauna-client-delegation
    #[must_use]
    pub fn with_lease_gate(mut self, gate: Arc<AtomicBool>) -> Self {
        self.lease_gate = Some(gate);
        self
    }

    /// The launch catch-up walk is behind us: from here every doc the seam
    /// delivers is the receive-path trickle, which the lease never gates.
    ///
    /// Idempotent and **per kind** — a builder outlives no session. One-way
    /// except through [`Self::reopen_catch_up`]: a kind whose backlog could not
    /// be walked when its boundary closed gets its window back before that
    /// backlog arrives. A kind this builder does not stage is ignored. Driven by
    /// `observe_catch_up_complete`.
    pub fn end_catch_up(&self, kind: ContentKind) {
        self.catching_up.lock().unwrap().remove(&kind);
    }

    /// Put `kind` back inside its catch-up window, so what the seam delivers
    /// next counts as backlog again until the next [`Self::end_catch_up`].
    ///
    /// The one caller is a community room's key-in landing after the
    /// Conversation boundary closed without that room
    /// (`MessageIndexObserver::observe_catch_up_reopened`). A kind this builder
    /// does not stage is ignored, exactly as the boundary ignores it — reopening
    /// a window this builder never had would withhold nothing it stages.
    pub fn reopen_catch_up(&self, kind: ContentKind) {
        if self.kinds.contains(&kind) {
            self.catching_up.lock().unwrap().insert(kind);
        }
    }

    /// Whether this builder still counts what it is handed for `kind` as launch
    /// backlog.
    pub fn is_catching_up(&self, kind: ContentKind) -> bool {
        self.catching_up.lock().unwrap().contains(&kind)
    }

    /// Whether a doc of `kind` offered *right now* would be withheld by the
    /// lease.
    ///
    /// Exactly one situation: this seat is standing down (a gate is attached and
    /// closed) **and** the work in front of it is the re-presenting kind (that
    /// kind's launch catch-up walk). A stood-down seat past a kind's catch-up
    /// boundary stages that kind normally — the ruled trickle carve-out, not an
    /// oversight — and it keeps withholding a *sibling* kind still mid-walk,
    /// which is the whole reason the window is per kind.
    fn withheld_by_lease(&self, kind: ContentKind) -> bool {
        match &self.lease_gate {
            None => false,
            Some(gate) => self.is_catching_up(kind) && !gate.load(Ordering::Acquire),
        }
    }

    /// Declare docs as already indexed, so [`Self::stage`] skips them.
    ///
    /// Called by `resume_mail_builder` with the identities read out of the
    /// segments this actor has already published — see the [`Self::indexed`]
    /// field for why that is load-bearing rather than an optimisation. A
    /// builder that is never seeded (a fresh actor, or the raw [`Self::new`] /
    /// [`Self::with_manifest`] constructors in tests) simply has nothing to
    /// skip, which is the correct first-run behaviour; production glue goes
    /// through `resume_mail_builder` and gets the seeding for free.
    ///
    /// Takes [`DocIdentity`] rather than [`ContentId`] because the guard keys
    /// on the nest record id too: seeding ids alone would answer "already
    /// indexed" for a Message-ID whose content a `Sent`-copy displacement has
    /// since replaced, which is the whole failure the record id removes.
    pub fn seed_indexed(&self, docs: impl IntoIterator<Item = DocIdentity>) {
        let mut indexed = self.indexed.lock().unwrap();
        indexed.extend(docs.into_iter().map(|d| (d.content_id.0, d.secondary_id)));
    }

    /// How many content ids this builder considers already indexed.
    pub fn indexed_len(&self) -> usize {
        self.indexed.lock().unwrap().len()
    }

    /// Shrink the segment ceiling so a test can drive the halving path with a
    /// handful of small docs instead of 8 MiB of them.
    #[cfg(test)]
    fn with_segment_ceiling(mut self, bytes: usize) -> Self {
        self.max_segment_bytes = bytes;
        self
    }

    /// How many observed docs are waiting for the next flush.
    pub fn pending_len(&self) -> usize {
        self.pending.lock().unwrap().len()
    }

    /// Record that mail up to `cursor` has been ingested — the advisory per-kind
    /// cursor (`content-index.md` § Ingest triggers, v1: *cursors are advisory;
    /// idempotence is structural*). Takes effect at the next flush, which is
    /// what publishes the manifest.
    ///
    /// Note the cursor is one `u64` for all of `ContentKind::Mail`, while INBOX
    /// and Sent are independent UID spaces — so a caller must not feed it raw
    /// per-mailbox UIDs from both feeds and expect either to survive the
    /// max-merge. That mismatch is one reason the re-index guard the corrected
    /// § Ingest triggers, v1 calls for leans toward a content-id check rather
    /// than a UID cursor.
    pub fn note_ingest_cursor(
        &self,
        kind: ContentKind,
        cursor: u64,
    ) -> Result<(), IndexBuildError> {
        self.manifest
            .lock()
            .unwrap()
            .merge_ingest_cursor(kind, cursor)?;
        Ok(())
    }

    /// The advisory cursor a resumed builder should start `kind`'s backfill
    /// from.
    pub fn ingest_cursor(&self, kind: ContentKind) -> Option<u64> {
        self.manifest.lock().unwrap().ingest_cursor(kind)
    }

    /// The advisory corpus marker recorded for `kind`, or `None` when no
    /// builder this manifest has seen has staged one.
    ///
    /// A third-ingest-class walk reads this to decide whether the corpus it is
    /// about to fetch is the one already staged. `None` means **unknown**, not
    /// *unchanged* — the walk must read the corpus.
    pub fn corpus_marker(&self, kind: ContentKind) -> Option<Vec<u8>> {
        self.manifest
            .lock()
            .unwrap()
            .corpus_marker(kind)
            .map(|m| m.to_vec())
    }

    /// Record the corpus marker for `kind`. Takes effect at the next flush,
    /// which is what publishes the manifest — so a walk that stages a corpus
    /// and notes its marker in the same breath has both land together, and a
    /// crash between them costs one redundant re-read on the next attach.
    pub fn note_corpus_marker(
        &self,
        kind: ContentKind,
        marker: Vec<u8>,
    ) -> Result<(), IndexBuildError> {
        self.manifest
            .lock()
            .unwrap()
            .set_corpus_marker(kind, marker)?;
        Ok(())
    }

    /// Stage the whole address-book corpus as this kind's snapshot.
    ///
    /// The contacts twin of [`Self::observe_draft_corpus`], and a method rather
    /// than a seam callback because the producer is the launcher's own
    /// reconcile walk (see [`IndexableContact`]). Everything
    /// [`Self::stage_snapshot`] documents applies unchanged — in particular
    /// that an **empty** corpus is a real event (the user deleted their last
    /// card) rather than a no-op.
    ///
    /// ⚠ The corpus is **per kind, across every address book**: staging one
    /// book's cards retires the segments holding the others', so the walk must
    /// assemble all books before calling this (`content-index.md` § Ingest
    /// triggers, v1 — the contacts ruling's last sub-bullet).
    ///
    /// Returns whether the corpus was actually staged — `false` when the lease
    /// withheld it (or the kind is not in this builder's set), so the caller's
    /// corpus marker must **not** advance: a marker over a corpus this seat
    /// never staged suppresses the walk until the next real corpus change.
    pub fn stage_contact_corpus(&self, contacts: &[IndexableContact]) -> bool {
        if !self.kinds.contains(&ContentKind::Contact) {
            return false;
        }
        self.stage_snapshot(
            ContentKind::Contact,
            contacts.iter().map(contact_doc_for).collect(),
        )
    }

    /// Stage the posts a reconcile walk found that this builder has not indexed
    /// yet — the **pass-shaped half** of the posts arm, so the advisory lease
    /// gates it exactly as it gates every launch backlog
    /// (`content-index.md` § Ingest triggers, v1 → the posts ruling; § Where the
    /// index is built → *the gate binds the pass-shaped part*).
    ///
    /// Returns whether the pass was admitted — `false` when the lease withheld
    /// it (or `Post` is not in this builder's set), so the walk's corpus marker
    /// must **not** advance and the next walk retries. `true` even when every
    /// item was already indexed or empty-bodied: the pass legitimately observed
    /// the corpus, and advancing the marker is what stops an unchanged corpus
    /// being re-paged on every sweep.
    ///
    /// Posts are append-shaped (`is_snapshot_kind` stays `false` — a post has
    /// no edit verb), so unlike the contacts corpus this goes through the
    /// re-index guard doc by doc: an id already indexed is dropped, never
    /// superseded.
    pub fn stage_posts_walk(&self, posts: &[IndexablePost]) -> bool {
        if !self.admits(ContentKind::Post) {
            return false;
        }
        if self.withheld_by_lease(ContentKind::Post) {
            return false;
        }
        for post in posts {
            self.stage_post_unchecked(post);
        }
        true
    }

    /// Stage the files a reconcile walk found that this builder has not indexed
    /// yet — the File arm's only producer (`content-index.md` § Ingest triggers,
    /// v1 → *The files/media arms are SCOPED*).
    ///
    /// The posts twin, minus the marker: File keeps **no corpus marker in v1**,
    /// and that is the ruled suppression rather than an omission. For an append
    /// kind the stage-time guard already makes an unchanged corpus stage nothing,
    /// and nothing staged publishes nothing — so a full drain per walk costs one
    /// paged wire read and zero rail bytes, which is the mail shape.
    ///
    /// There is **no trickle door beside this one**, also deliberately: file
    /// writers are out-of-process (sync agents, other devices, other members of a
    /// shared set), so the Media page's own upload is one writer among many and
    /// its record round-trips back as the `fauna.sync.changed` push anyway. Walk
    /// plus push only — the contacts posture.
    ///
    /// Returns whether the pass was admitted; `false` when the lease withheld it
    /// (or `File` is not in this builder's set). Nothing downstream depends on
    /// the answer today — there is no marker to hold back — but the door reports
    /// it for uniformity with the two that do, and so a marker added later cannot
    /// be advanced over a withheld stage.
    pub fn stage_files_walk(&self, files: &[IndexableFile]) -> bool {
        if !self.admits(ContentKind::File) {
            return false;
        }
        if self.withheld_by_lease(ContentKind::File) {
            return false;
        }
        for file in files {
            self.stage_file_unchecked(file);
        }
        true
    }

    /// The staging tail for one file: skip what has no renderable name, then the
    /// same guard + single-lock insert every append staging takes.
    ///
    /// **A path-less row is skipped without burning the re-index guard**, exactly
    /// as an empty-bodied post is. This is the arm's load-bearing degrade rather
    /// than a tidiness check: a row whose sealed label this seat cannot open
    /// arrives here empty (`SealedLabelRender::Omit` — not the label audience),
    /// and leaving its id unguarded is what lets a later walk
    /// on a seat that *does* hold the set's keys stage it for real.
    fn stage_file_unchecked(&self, file: &IndexableFile) {
        if file.path.is_empty() {
            return;
        }
        let doc = file_doc_for(file);
        if !self.admit_to_guard(&doc, false) {
            return;
        }
        self.pending.lock().unwrap().push(doc);
    }

    /// Stage the user's **own just-created post** — the trickle chokepoint,
    /// which the lease NEVER gates (`content-index.md` § Ingest triggers, v1 →
    /// the class template, piece 5: *a client-side create chokepoint, where one
    /// exists (posts), is trickle and stays ungated*).
    ///
    /// A separate door rather than a window state because posts never leave
    /// their catch-up window (the walk is permanently pass-shaped — no boundary
    /// signal exists for a corpus with no receive loop), so routing the trickle
    /// through [`Self::stage_doc`]'s lease check would withhold a stood-down
    /// seat's own new post for the life of the session — precisely the doc the
    /// ruling says every live builder stages regardless of who holds the lease.
    pub fn stage_post_trickle(&self, post: &IndexablePost) {
        if !self.admits(ContentKind::Post) {
            return;
        }
        self.stage_post_unchecked(post);
    }

    /// The shared tail of both posts doors: skip what has no text, then the
    /// same guard + single-lock insert every append staging takes.
    ///
    /// **An empty-bodied post (a video post) is skipped without burning the
    /// re-index guard**: it has nothing searchable, and leaving it unguarded is
    /// deliberate — the guard's set means "indexed", and lying in it would be
    /// the only cost, since re-considering the id costs nothing (walk rows
    /// arrive in pages that were fetched anyway).
    fn stage_post_unchecked(&self, post: &IndexablePost) {
        if post.text.is_empty() {
            return;
        }
        let doc = post_doc_for(post);
        if !self.admit_to_guard(&doc, false) {
            return;
        }
        self.pending.lock().unwrap().push(doc);
    }

    /// Buffer one message for the next flush. Public so any producer feeds the
    /// *same* path the receive hook does — one doc-shaping rule, one sink.
    ///
    /// A content id already indexed (published before this session, or staged
    /// earlier in it) is **dropped here** — see [`Self::indexed`]. Both the
    /// membership test and the insert happen under one lock, so two receive
    /// passes racing on the same message cannot both stage it.
    ///
    /// **The advisory lease is consulted here, and only for catch-up work**
    /// (`content-index.md` § Where the index is built → *The builder and the
    /// advisory task lease*). A stood-down seat drops the launch backlog — that
    /// work re-presents on every launch, so yielding it is retried by
    /// construction — but keeps staging live arrivals, because a doc on the
    /// trickle is seen once per session and dropping it would leave visible mail
    /// silently unsearchable on this seat.
    ///
    /// **A withheld doc is NOT recorded as indexed.** The lease check precedes
    /// the guard's insert on purpose: marking it would make this session's own
    /// mid-walk lease acquisition unable to pick the doc back up, converting an
    /// advisory yield into a permanent per-session loss — the opposite of
    /// "retried by construction".
    pub fn stage(&self, msg: &IndexableMessage<'_>) {
        // `is_own` is what licenses a supersession, and only it: see
        // `IndexableMessage::is_own` for why the index has to apply the
        // collision rule's asymmetry rather than simply following the content.
        self.stage_doc_inner(doc_for(msg), msg.is_own);
    }

    /// Buffer one already-shaped doc — the same path [`Self::stage`] funnels
    /// into, one step past the message-shaping.
    ///
    /// This is the entry point for a producer whose content is **not**
    /// message-shaped. `IndexableMessage` carries a thread id and a message id
    /// because the two rails it was built for (mail, conversations) have them;
    /// the remaining master kinds do not (a post has no thread, a contact has no
    /// body), so they shape their own [`IndexedDoc`] and hand it here rather
    /// than being forced through a message-shaped seam that would have to carry
    /// meaningless fields.
    ///
    /// Every rule [`Self::stage`] documents applies identically, because they
    /// are enforced here: the lease check, the re-index guard, and the
    /// single-lock test-and-insert.
    ///
    /// **Never supersedes.** A doc arriving through this door cannot replace
    /// one already indexed under its content id — only an *own* message copy
    /// may do that ([`Self::stage`]). Today that costs nothing: every producer
    /// on this door shapes its docs with `secondary_id: None`, so an id it has
    /// seen before matches and drops exactly as it always did. A future kind
    /// whose content id can be re-pointed at new content has to say so
    /// explicitly rather than inheriting mail's answer — the same discipline
    /// the mail guard itself owes drafts.
    pub fn stage_doc(&self, doc: IndexedDoc) {
        self.stage_doc_inner(doc, false);
    }

    fn stage_doc_inner(&self, doc: IndexedDoc, may_supersede: bool) {
        if !self.admits(doc.kind) {
            return;
        }
        if self.withheld_by_lease(doc.kind) {
            return;
        }
        if !self.admit_to_guard(&doc, may_supersede) {
            return;
        }
        self.pending.lock().unwrap().push(doc);
    }

    /// The re-index guard's decision for one doc: stage it or drop it, marking
    /// a supersession on the way through. The single door every append-side
    /// staging path goes through, so the three of them cannot drift.
    ///
    /// **Three answers, not two** (see the [`Self::indexed`] field for why the
    /// third exists): *unseen* → stage; *seen under the same nest record id* →
    /// drop, which is the ordinary every-launch re-walk; *seen under a
    /// different nest record id* → stage **and** record that a published
    /// segment holds a copy this doc supersedes, which the flush owes a
    /// rewrite. Only mail can reach the third arm today — every other producer
    /// shapes its docs with `secondary_id: None`, so `None == None` drops them
    /// exactly as before.
    ///
    /// The membership test, the supersession mark and the insert all happen
    /// under **one** acquisition of the guard lock, so two receive passes racing
    /// on one message cannot both stage it, and a flush racing this call sees
    /// either both the staged replacement and its rewrite mark or neither —
    /// never a replacement that quietly appends beside the copy it should have
    /// retired.
    fn admit_to_guard(&self, doc: &IndexedDoc, may_supersede: bool) -> bool {
        let mut indexed = self.indexed.lock().unwrap();
        match indexed.get(&doc.content_id.0) {
            Some(seen) if *seen == doc.secondary_id => return false,
            // Known under a different nest record, but this copy is not
            // authoritative — an `INBOX` record that is not the account's own.
            // Dropped, exactly as the conversations view drops it: the content
            // this id is *settled* to is the `Sent` copy's.
            Some(_) if !may_supersede => return false,
            Some(_) => {
                self.superseding
                    .lock()
                    .unwrap()
                    .insert(doc.content_id.0.clone());
                tracing::info!(
                    kind = ?doc.kind,
                    "index: an own copy arrived under a content id indexed from \
                     a different nest record — re-staging it and rewriting the \
                     segment that holds the superseded copy"
                );
            }
            None => {}
        }
        indexed.insert(doc.content_id.0.clone(), doc.secondary_id.clone());
        true
    }

    /// Stage the **entire current corpus** of a snapshot kind, replacing whatever
    /// this builder was holding for it.
    ///
    /// The counterpart of [`Self::stage_doc`] for a kind whose content is
    /// *edited and discarded* rather than appended (today: drafts —
    /// [`is_snapshot_kind`]). Three of `stage_doc`'s rules deliberately do not
    /// apply, each for the same underlying reason:
    ///
    /// - **The re-index guard is not consulted and not updated.** That guard
    ///   exists to stop a re-presenting *immutable* corpus from being
    ///   re-published every launch, and it works by never staging a known id
    ///   twice — which for a snapshot kind would pin the first version of every
    ///   draft forever. Bounding at-rest growth is instead the rewrite's own job
    ///   ([`Self::plan_flush`] supersedes the previous segment), and it bounds it
    ///   strictly better: to one copy of the corpus rather than one copy per
    ///   distinct content id ever seen.
    /// - **Prior pending docs of this kind are dropped, not appended to.** A
    ///   corpus supersedes a corpus; keeping both would seal two versions of the
    ///   same draft into one segment and leave which one wins to Tantivy's
    ///   intra-segment upsert ordering.
    /// - **An empty corpus is meaningful** — it is how "the user discarded their
    ///   last draft" reaches the index — so it stages a real, empty batch rather
    ///   than being skipped as a no-op.
    ///
    /// The lease check is unchanged and still applies: a stood-down seat
    /// withholds this corpus only while the kind is still in its catch-up
    /// window, exactly as for any other pass-shaped work.
    ///
    /// Returns whether the corpus was actually staged. `false` — withheld by
    /// the lease, or the kind is not this builder's — matters to any caller
    /// that stamps a corpus marker beside its stage: noting the marker over a
    /// withheld stage records a corpus this seat never staged, and the walk's
    /// precheck would then skip it until the next real corpus change.
    pub fn stage_snapshot(&self, kind: ContentKind, docs: Vec<IndexedDoc>) -> bool {
        debug_assert!(
            is_snapshot_kind(kind),
            "stage_snapshot is for snapshot kinds; {kind:?} appends"
        );
        if !self.admits(kind) {
            return false;
        }
        if self.withheld_by_lease(kind) {
            return false;
        }
        let mut pending = self.pending.lock().unwrap();
        pending.retain(|doc| doc.kind != kind);
        pending.extend(docs);
        self.snapshot_staged.lock().unwrap().insert(kind);
        true
    }

    /// Tokenize everything staged into one immutable segment **per kind**, seal
    /// each, publish them, then publish the single manifest that references them
    /// all. Returns an empty `Vec` when nothing was staged (the common case on a
    /// quiet timer tick).
    ///
    /// **Publish order is load-bearing: segment first, manifest second.** A
    /// crash between them leaves an unreferenced segment — invisible, harmless,
    /// and re-created on the next run because the advisory cursor never
    /// advanced past unpublished content. The reverse order would leave the
    /// manifest pointing at a blob that does not exist, which every reader
    /// would have to treat as corruption.
    ///
    /// **This is also where compaction happens** (`content-index.md` § Where
    /// the index is built, ruled 2026-08-02): once the kind is past the live
    /// segment threshold, the segment this flush would have *appended* is
    /// instead merged with small live segments and published as one, with the
    /// folded ids tombstoned in the same manifest write. The publish order,
    /// the wholesale manifest rewrite and the concurrency envelope are all
    /// unchanged — which is the whole reason the fold rides the flush rather
    /// than running as its own pass.
    pub async fn flush(&self) -> Result<Vec<SealedSegment>, IndexBuildError> {
        // Serialized against itself so the commit below cannot discard a
        // concurrent flush's manifest mutations (see `Self::flushing`).
        let _flushing = self.flushing.lock().await;

        // Which snapshot kinds delivered a corpus for this flush. Taken *before*
        // the batch, and separately from it, because an **empty** corpus is a
        // real event with no docs to carry it: discarding the last draft must
        // still tombstone the segment that holds the old one.
        let snapshot_staged: BTreeSet<ContentKind> =
            std::mem::take(&mut *self.snapshot_staged.lock().unwrap());

        // The supersessions this flush owes a segment rewrite, taken with the
        // same before-the-batch timing and for the same reason: they are an
        // *intent* that has to survive a failed flush, and a doc alone does not
        // carry it — re-staged without its mark, a replacement would append
        // beside the stale copy instead of retiring it.
        let superseding: BTreeSet<Vec<u8>> = std::mem::take(&mut *self.superseding.lock().unwrap());

        // Everything this flush took out of `pending`, accumulated so that any
        // failure below can put it back — nothing between here and the last
        // publish may drop a doc on the floor.
        let mut taken_docs: Vec<IndexedDoc> = Vec::new();
        let result = self
            .flush_inner(&snapshot_staged, &superseding, &mut taken_docs)
            .await;
        if result.is_err() {
            self.superseding.lock().unwrap().extend(superseding);
            self.restage(&snapshot_staged, taken_docs);
        }
        result
    }

    /// Whether this builder stages `kind` at all — the admission the observer
    /// doors have always applied, now shared with the direct ones.
    ///
    /// **Why the direct doors needed it.** `append_segment` refuses a doc of the
    /// wrong *class*, so a cross-class doc was never a risk. A **same-class doc
    /// outside this builder's kind set** was: it sealed, appended and published
    /// normally, while `catching_up` — seeded from the kind set at construction
    /// — never contained its kind, so [`Self::withheld_by_lease`] never gated it
    /// and the lease's stand-down could not hold it back. That is the
    /// indexed-but-unsearchable shape again, re-openable by a single mis-wired
    /// producer, and the query side would not answer for it either: a reader
    /// opens the kinds its own list names.
    ///
    /// Drop rather than error, matching
    /// [`Self::observe_indexable_message`]'s posture: a builder takes its own
    /// traffic off a shared seam, and every class's builder sees every kind.
    fn admits(&self, kind: ContentKind) -> bool {
        self.kinds.contains(&kind)
    }

    /// Re-stage everything a failed flush had taken, so the retry its caller
    /// logs is a real one.
    ///
    /// Front, not back, for the same reason `flush` re-splices a deferred tail
    /// there: these are the oldest docs, and the next flush should take them
    /// first. The re-index guard is deliberately **not** touched — these ids are
    /// still staged (they are back in `pending`), and un-marking them would let
    /// a concurrent re-presentation stage a second copy.
    fn restage(&self, snapshot_staged: &BTreeSet<ContentKind>, docs: Vec<IndexedDoc>) {
        // Staging runs off the flush guard, so a producer can have offered a
        // **fresher corpus** for a snapshot kind while this flush was publishing.
        // That corpus wins outright: splicing the stale one back would leave two
        // versions of the same draft in `pending` for one segment to seal, and
        // which survived would be Tantivy's intra-segment ordering — precisely
        // what `stage_snapshot`'s replace-don't-append rule exists to prevent.
        let refreshed: BTreeSet<ContentKind> = self
            .snapshot_staged
            .lock()
            .unwrap()
            .iter()
            .copied()
            .collect();

        let docs: Vec<IndexedDoc> = docs
            .into_iter()
            .filter(|d| !refreshed.contains(&d.kind))
            .collect();
        if !docs.is_empty() {
            // Front, not back, for the same reason `flush` re-splices a deferred
            // tail there: these are the oldest docs.
            self.pending.lock().unwrap().splice(0..0, docs);
        }

        // A snapshot kind's *intent to supersede* is part of what was taken: an
        // empty corpus carries no docs at all, so without this a failed flush
        // would silently forget that the user discarded their last draft.
        let mut staged = self.snapshot_staged.lock().unwrap();
        for kind in snapshot_staged {
            if !refreshed.contains(kind) {
                staged.insert(*kind);
            }
        }
    }

    /// The fallible body of [`Self::flush`], split out so every `?` below routes
    /// through one restore point rather than each error path remembering to.
    async fn flush_inner(
        &self,
        snapshot_staged: &BTreeSet<ContentKind>,
        superseding: &BTreeSet<Vec<u8>>,
        taken_docs: &mut Vec<IndexedDoc>,
    ) -> Result<Vec<SealedSegment>, IndexBuildError> {
        // One segment per kind, so the batch is grouped before anything is
        // sealed: a segment holds exactly one kind's docs, which is what lets the
        // manifest keep an independent id chain per kind.
        let mut sealed_per_kind: Vec<(ContentKind, Option<SealedBatch>, bool, Vec<IndexedDoc>)> =
            Vec::new();
        for (kind, docs) in self.take_batch() {
            // Which of this kind's staged docs replace one a published segment
            // already holds. **Copied, not moved out of the batch**: within one
            // seal `add_doc`'s upsert already collapses a replacement onto the
            // stale copy beside it, so the batch stays correct on its own, and
            // holding them out would lose the replacement entirely whenever no
            // published segment turns out to hold the id (the same-session
            // case, where the stale copy is in this very batch). The rewrite
            // below re-adds them over the *published* holder instead, which is
            // the only reach `add_doc` does not have by itself.
            let replacements: Vec<IndexedDoc> = docs
                .iter()
                .filter(|d| superseding.contains(&d.content_id.0))
                .cloned()
                .collect();
            // A *seal* failure still forfeits its batch, and deliberately so:
            // the failure is in tokenizing or wrapping these very docs, so
            // re-staging them would wedge the builder retrying the same failure
            // — the same argument the over-ceiling single doc is dropped on.
            // What this restore covers is the *publish* failure, which says
            // nothing about the docs and is the shape both probes hit.
            let mut bounded = self.seal_bounded(docs)?;
            // Exactly what this flush is carrying: `deferred` has already gone
            // back to `pending` below, and an over-ceiling doc was dropped.
            taken_docs.append(&mut bounded.sealed_docs);
            let complete = bounded.deferred.is_empty();
            if !complete {
                // Front, not back: the docs that did not fit this segment are the
                // oldest, and the next flush should take them first.
                self.pending.lock().unwrap().splice(0..0, bounded.deferred);
            }
            // Supersede only a corpus that arrived whole. A snapshot kind big
            // enough to trip the byte ceiling would otherwise replace its own
            // complete previous segment with a partial one — losing drafts from
            // the index rather than merely delaying them. Falling back to an
            // append leaves stale copies queryable until the next flush carries
            // the whole corpus, which is the pre-existing, non-destructive
            // failure mode.
            let supersede = is_snapshot_kind(kind) && snapshot_staged.contains(&kind) && complete;
            sealed_per_kind.push((kind, bounded.segment, supersede, replacements));
        }

        // A staged corpus that produced no docs at all: no segment to publish,
        // but its predecessors must still stop being live.
        for kind in snapshot_staged {
            if !sealed_per_kind.iter().any(|(k, _, _, _)| k == kind) {
                sealed_per_kind.push((*kind, None, true, Vec::new()));
            }
        }

        // Best-effort by construction: any reason not to fold falls through to
        // the ordinary append, which is always correct. Planned per kind and
        // entirely off the manifest lock, because it does rail I/O.
        let mut prepared: Vec<(ContentKind, Option<SealedBatch>, Option<PreparedFold>, bool)> =
            Vec::with_capacity(sealed_per_kind.len());
        for (kind, segment, supersede, replacements) in sealed_per_kind {
            // A superseding flush needs no compaction: it is already publishing
            // the kind's entire corpus as one segment and tombstoning the rest,
            // which is a stronger fold than the compactor could plan — and
            // folding *into* it would merge the stale copies it exists to retire
            // straight back in.
            let fold = if supersede {
                None
            } else {
                self.prepare_fold(kind, segment.as_ref(), replacements)
                    .await?
            };
            prepared.push((kind, segment, fold, supersede));
        }

        let Some(plan) = self.plan_flush(prepared)? else {
            return Ok(Vec::new());
        };

        // **Every segment first, the manifest last** — the same load-bearing
        // order as the single-kind flush, and for the same reason: a crash
        // part-way leaves unreferenced segments (invisible, harmless,
        // re-created), where a manifest written first could name a blob that
        // does not exist.
        for seg in &plan.segments {
            self.publish(&seg.path, &seg.bytes).await?;
        }
        // The manifest path follows the *class*, exactly as the seal does —
        // taking both from the one key is what stops a master-class builder
        // from publishing its manifest over `manifest-mailcal.idx`. One write,
        // however many kinds this flush touched.
        self.publish(&self.key.manifest_path(), &plan.manifest_bytes)
            .await?;

        // **Commit only now.** Everything above could still have failed, and
        // until this line the builder's live manifest is exactly what it was
        // before the flush — so a failure leaves no trace to be published
        // durably by the next one. Cheap: one whole-manifest move, and the
        // `flushing` guard is what makes it safe to write back a value planned
        // off a clone.
        *self.manifest.lock().unwrap() = plan.manifest;
        // The below-format retirements this plan applied are now durable; a failure
        // above would have left them queued for the next flush instead.
        self.stale_retire.lock().unwrap().clear();

        Ok(plan
            .segments
            .into_iter()
            .map(|seg| SealedSegment {
                kind: seg.kind,
                path: seg.path,
                byte_len: seg.bytes.len(),
                doc_count: seg.doc_count,
            })
            .collect())
    }

    /// The async half of fold-at-flush: decide whether this flush folds and, if
    /// so, produce the merged segment's sealed bytes.
    ///
    /// Runs entirely **outside** the manifest lock (it does rail I/O), taking
    /// only a snapshot of the live segment ids. That snapshot can be stale by
    /// the time [`Self::plan_flush`] commits, and deliberately nothing is done
    /// about it: `tombstone_segment` no-ops on an id that is no longer live,
    /// the merged segment's docs are dedupped against any surviving copy by
    /// `(kind, content_id)` at query time, and no blob is ever deleted — so the
    /// worst outcome of a race is redundant bytes at rest, never a wrong or
    /// missing result. That is the ruled "no new race class".
    ///
    /// `Ok(None)` means "do not fold this flush" — not eligible, nothing small
    /// enough to fold under the one-blob ceiling, or the merge did not actually
    /// fit. Every one of those is a designed outcome, retried at the next
    /// threshold crossing.
    ///
    /// **`replacements` turns the fold into a rewrite** (2026-09-16,
    /// `content-index-ingest.md` § Ingest triggers, v1 → *A mail content id is
    /// a stable identity, not immutable content*). These are docs whose content
    /// id a published segment already holds under a different nest record — a
    /// `Sent` copy displacing an `INBOX` squat — so the flush must retire the
    /// stale copy, not merely out-number it. Three things change when the list
    /// is non-empty: the compaction threshold does not gate (this fold is owed,
    /// not opportunistic), the inputs are exactly the segments *holding* a
    /// replaced id rather than the compactor's cheapest set, and the merge goes
    /// through [`Index::rebuild_with`] so each replacement's own `delete_term`
    /// lands on the copied-in segment. Everything downstream is unchanged: one
    /// new segment published, its inputs tombstoned, tombstone-only as always.
    async fn prepare_fold(
        &self,
        kind: ContentKind,
        staged: Option<&SealedBatch>,
        replacements: Vec<IndexedDoc>,
    ) -> Result<Option<PreparedFold>, IndexBuildError> {
        let live: Vec<u32> = {
            let manifest = self.manifest.lock().unwrap();
            match manifest.kind(kind) {
                Some(km) => km.live_segments.clone(),
                None => return Ok(None),
            }
        };
        if !replacements.is_empty() {
            return self
                .prepare_rewrite(kind, &live, staged, replacements)
                .await;
        }
        if live.len() <= COMPACTION_LIVE_SEGMENT_THRESHOLD {
            return Ok(None);
        }

        let staged_bytes = staged.map_or(0, |s| s.bytes.len() as u64);
        let listing = self.publisher.list_entries().await?;

        // The manifest is the authority on what is live; the rail is the
        // authority on what exists. A live id the rail has not replicated yet
        // is simply not a fold candidate — the same posture `open_mailcal`
        // takes when it opens the index.
        let candidates: Vec<FoldCandidate> = live
            .iter()
            .filter_map(|&seg_id| {
                let path = segment_path(kind, seg_id);
                let entry = listing.iter().find(|e| e.path == path)?;
                Some(FoldCandidate {
                    seg_id,
                    sealed_bytes: entry.size_bytes,
                })
            })
            .collect();

        let Some(plan) = plan_fold(&candidates, staged_bytes, self.max_segment_bytes as u64) else {
            return Ok(None);
        };

        // Open the fold inputs, plus the batch this flush already sealed. The
        // staged segment is unsealed rather than re-tokenized: it is our own
        // blob, seconds old, and one AEAD open is far cheaper than tokenizing
        // the whole batch a second time.
        let mut plaintexts: Vec<Vec<u8>> = Vec::with_capacity(plan.inputs.len() + 1);
        if let Some(batch) = staged {
            plaintexts.push(self.key.open_segment_bytes(&batch.bytes)?);
        }
        for seg_id in &plan.inputs {
            let path = segment_path(kind, *seg_id);
            let Some(entry) = listing.iter().find(|e| e.path == path) else {
                continue;
            };
            let sealed = self.publisher.fetch_blob(&entry.blob_hash).await?;
            plaintexts.push(self.key.open_segment_bytes(&sealed)?);
        }

        let merged = Index::merge_segments(&plaintexts)?;
        let sealed = self.key.seal_segment_bytes(&merged)?;

        // The planner budgets on sealed input sizes, which is a proxy: the real
        // merged size is only known now. Over the ceiling means the rail could
        // not carry the result, so this flush appends instead and the fold is
        // re-attempted next time — never a segment that cannot be published.
        if sealed.len() > self.max_segment_bytes {
            tracing::debug!(
                merged_bytes = sealed.len(),
                ceiling = self.max_segment_bytes,
                "index: fold would exceed the segment ceiling — appending instead"
            );
            return Ok(None);
        }

        // One scan of the merged result, so the reported count describes the
        // segment actually published rather than just the staged batch. Paid
        // once per fold (episodic, and already off the receive path), not per
        // message.
        let doc_count = Index::open_from_bytes(&merged)?.content_ids()?.len();

        Ok(Some(PreparedFold {
            inputs: plan.inputs,
            sealed,
            doc_count,
        }))
    }

    /// Rewrite the published segments holding a **superseded** content id, so
    /// the stale copy stops matching.
    ///
    /// The mail arm's answer to the Message-ID collision rule
    /// (`content-index-ingest.md` § Ingest triggers, v1 → *A mail content id is
    /// a stable identity, not immutable content*). A `Sent` copy that displaced
    /// an `INBOX` squat carries the same Message-ID as a doc some earlier flush
    /// sealed, and no amount of re-staging retires that doc: `add_doc`'s upsert
    /// stops at the boundary of one sealed segment
    /// (`fauna-index/tests/cross_segment_supersession.rs`). Opening the holder
    /// as an *input* is the reach it lacks — [`Index::rebuild_with`].
    ///
    /// **Why the whole live set is read back.** The manifest records which
    /// segments are live, never which content ids each holds, so the holder is
    /// found by opening them. That is real rail I/O, and it is affordable only
    /// because it is rare: `superseding` is empty in every flush but one that
    /// actually carries a displacement, and a displacement needs a forged
    /// `INBOX` record squatting on a Message-ID this account also sent under.
    ///
    /// **Not gated on the compaction threshold**, unlike an opportunistic fold:
    /// this rewrite is *owed*. It is bounded instead by the holders themselves
    /// — each was already under the one-blob ceiling and comes back one doc
    /// lighter and one heavier — and an over-ceiling result declines to the
    /// ordinary append, the same non-destructive fallback a snapshot kind's
    /// over-ceiling corpus takes: the stale copy stays queryable until a later
    /// flush retires it, which is strictly better than a segment the rail
    /// cannot carry.
    async fn prepare_rewrite(
        &self,
        kind: ContentKind,
        live: &[u32],
        staged: Option<&SealedBatch>,
        replacements: Vec<IndexedDoc>,
    ) -> Result<Option<PreparedFold>, IndexBuildError> {
        let replaced: BTreeSet<Vec<u8>> = replacements
            .iter()
            .map(|d| d.content_id.0.clone())
            .collect();
        let listing = self.publisher.list_entries().await?;

        // The manifest is the authority on what is live; the rail is the
        // authority on what exists — the same posture `prepare_fold` takes.
        let mut inputs: Vec<u32> = Vec::new();
        let mut plaintexts: Vec<Vec<u8>> = Vec::new();
        for &seg_id in live {
            let path = segment_path(kind, seg_id);
            let Some(entry) = listing.iter().find(|e| e.path == path) else {
                continue;
            };
            let sealed = self.publisher.fetch_blob(&entry.blob_hash).await?;
            let plain = self.key.open_segment_bytes(&sealed)?;
            let holds = Index::open_from_bytes(&plain)?
                .content_ids()?
                .into_iter()
                .any(|id| replaced.contains(&id.0));
            if holds {
                inputs.push(seg_id);
                plaintexts.push(plain);
            }
        }

        if inputs.is_empty() {
            // Nothing published holds the id, so the stale copy — if there is
            // one at all — is inside this very batch, where `add_doc`'s upsert
            // has already collapsed it. The ordinary append is exactly right.
            return Ok(None);
        }

        // The batch rides along for the same reason it does in a fold: its docs
        // would otherwise be published twice, once in its own segment and once
        // inside the rewrite. `plan_flush`'s fold arm drops the batch append
        // whenever a fold is present, so the rewrite must carry it.
        if let Some(batch) = staged {
            plaintexts.insert(0, self.key.open_segment_bytes(&batch.bytes)?);
        }

        let rebuilt = Index::rebuild_with(&plaintexts, replacements)?;
        let sealed = self.key.seal_segment_bytes(&rebuilt)?;
        if sealed.len() > self.max_segment_bytes {
            tracing::debug!(
                rebuilt_bytes = sealed.len(),
                ceiling = self.max_segment_bytes,
                "index: superseding rewrite would exceed the segment ceiling — \
                 appending instead; the stale copy stays queryable until a \
                 later flush retires it"
            );
            return Ok(None);
        }

        let doc_count = Index::open_from_bytes(&rebuilt)?.content_ids()?.len();
        tracing::info!(
            kind = ?kind,
            segments = inputs.len(),
            "index: rewrote the segments holding a superseded content id"
        );
        Ok(Some(PreparedFold {
            inputs,
            sealed,
            doc_count,
        }))
    }

    /// Group the staged docs by kind, taking at most [`MAX_DOCS_PER_SEGMENT`]
    /// **per kind** and leaving the rest for the next flush — so a large
    /// backfill produces a segment chain rather than one blob the rail could not
    /// carry.
    ///
    /// The cap is per kind because the segment it bounds is per kind: one busy
    /// kind must not consume a quiet sibling's room and defer its docs for no
    /// reason. Relative order is preserved within each kind, and the overflow
    /// goes back to `pending` in order, so the next flush takes the oldest
    /// first.
    fn take_batch(&self) -> BTreeMap<ContentKind, Vec<IndexedDoc>> {
        let mut pending = self.pending.lock().unwrap();
        let mut taken: BTreeMap<ContentKind, Vec<IndexedDoc>> = BTreeMap::new();
        let mut overflow: Vec<IndexedDoc> = Vec::new();
        for doc in std::mem::take(&mut *pending) {
            let slot = taken.entry(doc.kind).or_default();
            // A snapshot kind's batch is not split: the segment it produces
            // *replaces* the kind's previous one, so half a corpus is not a
            // smaller correct answer — it is the other half going missing. The
            // byte ceiling still applies (`seal_bounded`), and a corpus that
            // trips it declines to supersede rather than publishing a partial
            // one (see `flush`).
            if is_snapshot_kind(doc.kind) || slot.len() < MAX_DOCS_PER_SEGMENT {
                slot.push(doc);
            } else {
                overflow.push(doc);
            }
        }
        *pending = overflow;
        taken
    }

    /// Tokenize and seal `docs` into one segment no larger than
    /// [`MAX_SEGMENT_BYTES`].
    ///
    /// The doc cap alone cannot guarantee the byte ceiling (a handful of very
    /// large messages can outweigh thousands of small ones), so an oversize
    /// seal halves the batch and retries, handing the tail back to be re-staged.
    /// A *single* doc that still exceeds the ceiling is **dropped with a
    /// warning**: its message is untouched on the nest and the doc is
    /// re-derivable, so what is lost is one entry's searchability — never
    /// content, and never a builder wedged retrying the same doc forever.
    fn seal_bounded(&self, mut docs: Vec<IndexedDoc>) -> Result<BoundedSeal, IndexBuildError> {
        let mut deferred: Vec<IndexedDoc> = Vec::new();
        loop {
            if docs.is_empty() {
                return Ok(BoundedSeal {
                    segment: None,
                    deferred,
                    sealed_docs: Vec::new(),
                });
            }
            let sealed = self.seal_docs(&docs)?;
            if sealed.len() <= self.max_segment_bytes {
                return Ok(BoundedSeal {
                    segment: Some(SealedBatch {
                        doc_count: docs.len(),
                        bytes: sealed,
                    }),
                    deferred,
                    sealed_docs: docs,
                });
            }
            if docs.len() == 1 {
                tracing::warn!(
                    sealed_bytes = sealed.len(),
                    ceiling = self.max_segment_bytes,
                    "index: one document seals larger than a segment may be; \
                     skipping it — the message itself is untouched and the doc \
                     is re-derivable"
                );
                // Deliberately NOT handed back for retry: this doc can never
                // seal under the ceiling, so re-staging it would wedge the
                // builder retrying it forever. Dropping it costs one entry's
                // searchability, exactly as the warning says.
                return Ok(BoundedSeal {
                    segment: None,
                    deferred,
                    sealed_docs: Vec::new(),
                });
            }
            let tail = docs.split_off(docs.len() / 2);
            deferred.splice(0..0, tail);
        }
    }

    /// Tokenize `docs` into a fresh in-RAM index and seal it under this
    /// builder's class key.
    fn seal_docs(&self, docs: &[IndexedDoc]) -> Result<Vec<u8>, IndexBuildError> {
        let mut index = Index::create_in_ram()?;
        for doc in docs {
            index.add_doc(doc.clone())?;
        }
        index.commit()?;
        Ok(self.key.seal_index(&mut index)?)
    }

    /// The manifest half of a flush: take the next segment id and serialize the
    /// manifest that lists it — both under the manifest lock, so the id a
    /// segment was assigned and the manifest that lists it can never disagree.
    ///
    /// Split out from [`Self::flush`] so the `MutexGuard` provably never
    /// crosses an await: it is not `Send`, and holding a std lock across one is
    /// a deadlock waiting to happen. `None` means there is nothing to publish.
    /// `prepared` carries one entry per kind this flush touched: what it sealed
    /// (if anything) and whether it folds. Every kind's segment id is assigned
    /// and every tombstone applied under **one** acquisition of the lock, then
    /// the single manifest is serialized once — which is what makes "one
    /// manifest per class" hold no matter how many kinds a flush spans.
    fn plan_flush(
        &self,
        prepared: Vec<(ContentKind, Option<SealedBatch>, Option<PreparedFold>, bool)>,
    ) -> Result<Option<FlushPlan>, IndexBuildError> {
        // **Planned on a clone, never in place.** Every mutation below — id
        // assignment, fold tombstones, supersession — is speculative until the
        // whole flush has published; `flush` commits this back only then. See
        // `FlushPlan::manifest`.
        let mut manifest = self.manifest.lock().unwrap().clone();
        let mut segments: Vec<PlannedSegment> = Vec::new();
        let mut superseded_any = false;

        // Below-format retirements queued at resume (`Self::retire_segments`) go
        // first, so a fold planned in this same flush can never have chosen a
        // stale blob as an input *after* it stopped being live — and so the
        // "nothing to seal" early-out below still publishes a manifest that
        // carries only retirements. Read, not drained: the queue is cleared
        // after the commit, so a failed publish retries these.
        let stale: Vec<(ContentKind, u32)> = self.stale_retire.lock().unwrap().clone();
        let retired_any = !stale.is_empty();
        for (kind, seg_id) in stale {
            manifest.tombstone_segment(kind, seg_id);
        }

        for (kind, sealed, fold, supersede) in prepared {
            // A snapshot kind's new segment holds the whole corpus, so every
            // segment that was live before it is now a strictly older copy of
            // the same content: an edited draft's previous text, and any draft
            // since discarded. Retiring them here is what the append-side
            // re-index guard cannot do — `Index::add_doc`'s upsert stops at the
            // boundary of one sealed segment (pinned by
            // `fauna-index/tests/cross_segment_supersession.rs`), so a stale
            // copy is unreachable by any amount of re-staging and survives
            // compaction. Tombstone-only, like every other retirement: the
            // blobs stay at rest and in sync (`content-index.md` § Where the
            // index is built).
            if supersede {
                let live: Vec<u32> = manifest
                    .kind(kind)
                    .map(|km| km.live_segments.clone())
                    .unwrap_or_default();
                for seg_id in live {
                    manifest.tombstone_segment(kind, seg_id);
                }
                superseded_any = true;
            }
            // A fold *replaces* the append: the staged batch's docs are already
            // inside the merged segment, so publishing both would double them.
            if let Some(PreparedFold {
                inputs,
                sealed,
                doc_count,
            }) = fold
            {
                let seg_id = manifest.append_segment(kind)?;
                for folded in &inputs {
                    // Tombstone-only, always: the folded blobs stay at rest and
                    // in sync, and physical reclamation is the deferred
                    // distributed-GC question (`content-index.md` § Where the
                    // index is built). A `false` here means the id stopped being
                    // live while we were off the lock doing rail I/O — a
                    // designed race outcome.
                    manifest.tombstone_segment(kind, *folded);
                }
                segments.push(PlannedSegment {
                    kind,
                    path: segment_path(kind, seg_id),
                    bytes: sealed,
                    doc_count,
                });
                continue;
            }

            let Some(SealedBatch { bytes, doc_count }) = sealed else {
                continue;
            };

            // `append_segment` is class-guarded — a kind outside this manifest's
            // class is refused here rather than silently wrapped under the wrong
            // key. The constructor's assert makes that unreachable for a builder
            // built correctly; this is the second belt.
            let seg_id = manifest.append_segment(kind)?;
            segments.push(PlannedSegment {
                kind,
                path: segment_path(kind, seg_id),
                bytes,
                doc_count,
            });
        }

        if segments.is_empty() && !superseded_any && !retired_any {
            // Nothing new to seal. A cursor advance staged by the backfill still
            // has to reach the folder, so publish the manifest alone; with no
            // cursor on any of this builder's kinds either, a quiet timer tick
            // writes nothing at all.
            //
            // A supersession is the other reason to write a manifest with no
            // segment: discarding the last draft retires a segment and publishes
            // nothing, and skipping the write would leave the retired copy live
            // and the discarded draft searchable. A below-format retirement is the
            // third — the tombstones must land even if this seat stages
            // nothing, or the stale segments stay live for every reader.
            if !self
                .kinds
                .iter()
                .any(|k| manifest.ingest_cursor(*k).is_some())
            {
                return Ok(None);
            }
        }

        Ok(Some(FlushPlan {
            manifest_bytes: self.key.seal_manifest(&manifest)?,
            segments,
            manifest,
        }))
    }

    async fn publish(&self, path: &str, bytes: &[u8]) -> Result<(), IndexBuildError> {
        self.publisher
            .publish(path, bytes)
            .await
            .map_err(|e| match e {
                IndexBuildError::Publish { reason, .. } => IndexBuildError::Publish {
                    path: path.to_string(),
                    reason,
                },
                other => other,
            })
    }
}

impl MessageIndexObserver for IndexBuilder {
    fn observe_indexable_message(&self, msg: IndexableMessage<'_>) {
        // A builder stages the kinds of its own class and drops everything else.
        // This is not a stub or a rollout gate: a builder holds exactly one
        // class's key, so it *could not* seal another class's doc correctly even
        // if it wanted to (`content-index.md` § Don't do these — never wrap a
        // master-key kind under a key a MUA credential reaches). Every class's
        // builder is registered on the same seam and each takes its own traffic.
        if !self.kinds.contains(&content_kind_of(msg.kind)) {
            return;
        }
        self.stage(&msg);
    }

    fn observe_catch_up_complete(&self, kind: IndexableKind) {
        // Retires exactly the named kind's window — never a sibling's, even one
        // this same builder stages. A boundary describes one kind's backlog, so
        // closing another's on it would reclassify that other kind's remaining
        // walk as trickle, which the lease never gates: mail's sweep ending the
        // Conversation window mid-refold, or a permanently erroring mail feed
        // holding it open all session (`content-index.md` § Ingest triggers, v1
        // — *The boundary signal is per kind*). A kind this builder does not
        // stage is dropped by `end_catch_up` itself.
        self.end_catch_up(content_kind_of(kind));
    }

    fn observe_catch_up_reopened(&self, kind: IndexableKind) {
        // The boundary's inverse, and per kind for the same reason: reopening a
        // sibling's window too would have a stood-down seat withhold that
        // sibling's live trickle until some boundary it is not waiting on.
        self.reopen_catch_up(content_kind_of(kind));
    }

    fn observe_draft_corpus(&self, drafts: &[IndexableDraft]) {
        if !self.kinds.contains(&ContentKind::Draft) {
            return;
        }
        self.stage_snapshot(
            ContentKind::Draft,
            drafts.iter().map(draft_doc_for).collect(),
        );
    }
}

/// Whether a kind is built by **replacing its whole corpus** rather than by
/// appending to it (`content-index.md` § Ingest triggers, v1 → *Drafts are a
/// snapshot kind*).
///
/// The test is not "is it small" but **"can its content change IN PLACE after
/// it is indexed"** (refined 2026-08-05 by the posts ruling —
/// `content-index.md` § Ingest triggers, v1). An append kind's content id
/// names something whose text never changes (an RFC `Message-ID`, an ingested
/// conversation message, a content-addressed post body), so yesterday's
/// segment is still true today. A snapshot kind's does not: a draft or a vCard
/// is rewritten under a stable identity, and no added document can retire the
/// stale text — `Index::add_doc`'s upsert cannot reach a segment that is
/// already sealed. So its builder republishes the corpus and retires what came
/// before, which is both exact and bounded. **Vanish-only is NOT enough for
/// snapshot**: a kind whose content can only be deleted (posts) stays
/// append-shaped, because the class resolver reads the live corpus at query
/// time and drops a hit whose content is gone, and the at-rest residue purges
/// at the next versioned re-index — where a whole-corpus rewrite per delete
/// would be pathological at post-history scale.
///
/// Total rather than a `matches!`, so a kind added to the format has to answer
/// the question: getting this wrong is silent in both directions — an append
/// kind treated as a snapshot loses everything not in the latest batch, and a
/// snapshot kind treated as an append accumulates every version forever.
fn is_snapshot_kind(kind: ContentKind) -> bool {
    match kind {
        // Edited in place and discarded outright.
        ContentKind::Draft => true,
        // A vCard is edited in place under a stable identity (`uid_hash`) and a
        // CardDAV MUA can DELETE it outright — change-in-place, so the refined
        // test above says snapshot. Staged as a whole-corpus rewrite by the
        // ctag-prechecked reconcile walk (`content-index.md` § Ingest triggers,
        // v1 — the contacts ruling).
        ContentKind::Contact => true,
        // Immutable once ingested.
        ContentKind::Mail | ContentKind::Calendar | ContentKind::Conversation => false,
        // Append-shaped by the refined test above: both are vanish-only, and a
        // deletion heals at resolve time rather than by rewriting the corpus.
        //
        // File is the sharper case, because its text *derives from* its identity
        // — `path_hash = blake3(normalized path)` — so a file's searchable text
        // can never change while its identity lives. A content edit keeps both;
        // a **rename is structurally delete + create** (the engine's watcher
        // resolves renames by on-disk existence, and nothing links the two rows),
        // so the vanish-only test is met by the one verb that looked like
        // change-in-place (`content-index.md` § Ingest triggers, v1 → *The
        // files/media arms are SCOPED*).
        ContentKind::Post | ContentKind::File => false,
        // Not yet built, and refuted for v1 rather than merely unscheduled: the
        // one enumerable "media" corpus IS the file corpus, so a Media arm today
        // would double-index File. It activates with the classifier plans (7/8),
        // whose tokens are its real corpus.
        ContentKind::Media => false,
    }
}

/// Whether `kind`'s launch backlog **re-presents** — the one property that lets
/// a closed lease gate withhold it (`content-index.md` § Where the index is
/// built → *The builder and the advisory task lease*: "a stand-down may bind
/// only build work whose queue re-presents").
///
/// Every walk- or sweep-driven kind re-presents: the mail re-page from UID 0,
/// the Conversation attach walk over the thread store, the contacts / posts /
/// files reconcile walks at attach and on every sweep. **Draft does not.** Its
/// corpus reaches the builder only from `DraftStore` itself — once per compose
/// change and once when the observer is registered — and nothing ever offers it
/// again, so a withheld draft corpus is not "retried by construction"; it is
/// lost on this seat until the user's next keystroke. Found at code level
/// 2026-09-25 while building the phone-finds witnesses: the gate starts closed
/// until the lease loop's first observe lands, the registration re-offer races
/// it, and a draft written on a phone — restored by the desktop at launch and
/// indexed by no other seat — could lose that race silently. Stream-shaped work
/// is exactly what the ruling says the lease never gates, so Draft has no
/// catch-up window at all.
///
/// Total, like [`is_snapshot_kind`], so a kind added to the format has to
/// answer: getting this wrong is silent in the losing direction.
fn has_catch_up_window(kind: ContentKind) -> bool {
    match kind {
        // Offered once per change by its store; never re-presented.
        ContentKind::Draft => false,
        // Re-paged, re-walked or re-swept — yielding it is retried by
        // construction.
        ContentKind::Mail
        | ContentKind::Calendar
        | ContentKind::Conversation
        | ContentKind::Contact
        | ContentKind::Post
        | ContentKind::File
        | ContentKind::Media => true,
    }
}

/// Shape one draft into an indexable doc.
///
/// The draft's stable identity is its content id (the thread id, or the reserved
/// new-thread id) — **never** a hash of its text. An id that moved with the body
/// would make every edit a new document and leave every past version live
/// forever, which is precisely the growth the snapshot rewrite exists to bound.
///
/// **Timestamp: the observation, not the edit.** A `ComposeState` carries no
/// modification time, and adding one would change the `__drafts` at-rest shape
/// for a field only search would read. The corpus is re-published whole on every
/// change anyway, so "when this corpus was observed" is both available and
/// honest; it feeds only the merge's recency tie-break, never correctness
/// (`ui/search.md` § State & data shape — *Ordering*).
fn draft_doc_for(draft: &IndexableDraft) -> IndexedDoc {
    let mut fields = Vec::with_capacity(2);
    if let Some(subject) = draft.subject.as_deref().filter(|s| !s.is_empty()) {
        fields.push(IndexedField {
            kind: FieldKind::Title,
            text: subject.to_string(),
        });
    }
    if !draft.body.is_empty() {
        fields.push(IndexedField {
            kind: FieldKind::Body,
            text: draft.body.clone(),
        });
    }
    IndexedDoc {
        kind: ContentKind::Draft,
        content_id: ContentId(draft.content_id.as_bytes().to_vec()),
        timestamp_ns: observed_now_ns(),
        // A draft is the user's own unsent text: there is no sender to attribute
        // it to, and the author is always the actor whose index this is.
        sender_actor_id: None,
        secondary_id: None,
        fields,
    }
}

/// One address-book card, as the index needs it.
///
/// The producer is the launcher's reconcile walk rather than a seam callback
/// (`content-index.md` § Ingest triggers, v1 — the contacts ruling: the corpus
/// is nest-resident and externally mutated, so there is no client-side
/// chokepoint to hook), which is why this type lives beside the builder instead
/// of in `fauna_conversations::index_sink` the way [`IndexableDraft`] does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexableContact {
    /// The card's `uid_hash`, **hex-lowercase** — the index's `content_id`.
    ///
    /// Stable across in-place vCard edits, which is the property both
    /// `add_doc`'s upsert and the cross-device dedup key on, and it is already
    /// the wire's own dedup key (`fauna_client_carddav`). Lowercase because the
    /// nest's `content_id` conformance pin spells every hex id that way.
    pub uid_hash: String,
    /// The card's display name (vCard `FN`) → the index's `Title`.
    pub display_name: Option<String>,
    /// The rest of the card's searchable text → the index's `Body`.
    pub text: String,
}

/// Shape one card into its indexed doc.
///
/// **Timestamp: the observation, not the card's `REV`.** Same reasoning as
/// [`draft_doc_for`] — the corpus is republished whole on every change, so
/// "when this corpus was observed" is both available and honest, and it feeds
/// only the merge's recency tie-break (`ui/search.md` § State & data shape).
/// Using the card's own `internal_date` would rank a freshly imported address
/// book by how old its contacts are.
fn contact_doc_for(contact: &IndexableContact) -> IndexedDoc {
    let mut fields = Vec::with_capacity(2);
    if let Some(name) = contact.display_name.as_deref().filter(|s| !s.is_empty()) {
        fields.push(IndexedField {
            kind: FieldKind::Title,
            text: name.to_string(),
        });
    }
    if !contact.text.is_empty() {
        fields.push(IndexedField {
            kind: FieldKind::Body,
            text: contact.text.clone(),
        });
    }
    IndexedDoc {
        kind: ContentKind::Contact,
        content_id: ContentId(contact.uid_hash.as_bytes().to_vec()),
        timestamp_ns: observed_now_ns(),
        // A card is the user's own address-book entry — no sender to attribute,
        // exactly as for a draft.
        sender_actor_id: None,
        secondary_id: None,
        fields,
    }
}

/// One of the user's own posts, as either posts door stages it — a
/// `fauna.posts.list` row (the walk) or a just-confirmed create (the trickle).
///
/// Lives beside [`IndexableContact`] for the same reason it does: the producer
/// is launcher-owned rather than a seam callback (`content-index.md` § Ingest
/// triggers, v1 — the posts ruling), so the type belongs to the builder, not to
/// `fauna_conversations::index_sink`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexablePost {
    /// Hex-lowercase 32-byte post digest — the index's `content_id`, identical
    /// to `fauna.posts.get`'s `post_id` and to the spelling backend 1's rows
    /// carry, which is what lets the `(kind class, id)` dedup match the twins
    /// (`fauna_client_search::kind`).
    pub post_id: String,
    /// Epoch **microseconds** — `content.created_at`'s unit on the wire
    /// (`fauna_protocol::posts::PostsListItem`). The trickle, which never sees
    /// the nest-assigned value (`PostCreateReply` carries only the id), stamps
    /// its own observation instead — moments late, and feeding only the merge's
    /// recency tie-break.
    pub created_at_micros: i64,
    /// The post's plain body text, from `fauna_core::data::Post::body_text` on
    /// both doors — the same extraction that feeds the nest's own list rows and
    /// `content_fts`, so the two indexes can never disagree about what a post's
    /// text is.
    pub text: String,
}

/// One file in one of the user's readable folders, as the reconcile walk
/// stages it (`content-index.md` § Ingest triggers, v1 → *The files/media arms
/// are SCOPED*).
///
/// Lives beside [`IndexablePost`] for the same reason: the producer is the
/// launcher's own walk over `fauna.media.list`, not a seam callback, so the type
/// belongs to the builder rather than to `fauna_conversations::index_sink`.
///
/// **The searchable text is the path and nothing else.** File *bytes* rest
/// chunked and sealed, so content-text extraction would be a download +
/// decrypt + per-format parse pass — explicitly out of scope for v1 and a
/// classifier-adjacent question, not a blocker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexableFile {
    /// The owning set's **stable** `FolderSummary.id`, half of the doc
    /// identity.
    ///
    /// The row id rather than the set *name* because a name is user-renameable
    /// and `MediaItem` carries only the name: keying on it would move every file
    /// identity in a set the moment it was renamed, re-indexing the whole set
    /// and orphaning its old docs. The walk performs the name→id join once per
    /// pass so the identity it stamps outlives the rename.
    pub folder_id: i64,
    /// `blake3(normalized relative path)`, hex-lowercase — the other half of the
    /// identity (`fauna_core::sync::path_hash`, the wire's `MediaItem.path_hash`).
    ///
    /// Hex-lowercase to match every other id spelling in this subsystem, and
    /// paired with the set id because `path_hash` alone is a *set-relative* key
    /// and would collide across sets holding the same relative path.
    pub path_hash_hex: String,
    /// The file's set-relative path, forward-slash normalized — **rendered**,
    /// never the raw wire column.
    ///
    /// Post-scrub the nest rests no plaintext path for an ordinary set, so this
    /// is what `fauna_core::label_custody::render_path` returned under the set's
    /// download keys. A row this seat could not open never reaches here at all
    /// (the walk skips it), which is why an empty path here means "a file whose
    /// name is genuinely empty", not "unreadable".
    pub path: String,
    /// The file's last-change time in **unix seconds** — `MediaItem.updated_at`.
    ///
    /// ⚠ Observed at *first stage* and never refreshed: a File doc is
    /// append-shaped and the re-index guard declines the re-stage, so a later
    /// edit does not update it. Recency ordering pays a little; the rendered row
    /// does not, because live values come from the resolver (a cost the ruling
    /// states rather than leaves to be discovered).
    pub updated_at_secs: i64,
}

/// The doc identity spelling for a file: `"<folder_id>:<path_hash hex>"`.
///
/// One function so the walk (which mints it) and the resolver (which parses it
/// back out of a hit) can never disagree — the pair is joined with a `:` because
/// neither half can contain one: an i64's decimal rendering and a hex digest are
/// both `:`-free, so the split is unambiguous without escaping.
pub(crate) fn file_content_id(folder_id: i64, path_hash_hex: &str) -> String {
    format!("{folder_id}:{path_hash_hex}")
}

/// Shape one file into its indexed doc — the basename (and its stem) as `Title`,
/// the whole path (and the extension) as `Body`.
///
/// **Why four texts and not just the path.** The ruling asks for "basename and
/// segments, tokenized", and the shared tokenizer does not deliver that from the
/// path alone: `unicode_words` splits on `/`, so directory segments do become
/// their own tokens, but it treats `.` between letters as *word-internal*
/// (UAX #29 `MidNumLet`), so `holidays/albatross.jpg` tokenizes to exactly
/// `["albatross.jpg", "holidays"]` — and a user typing `albatross` matches
/// **nothing**. Pinned in `fauna_mail::tokenizer`'s own tests.
///
/// The fix belongs here rather than in the tokenizer, and that is the load-
/// bearing choice: the tokenizer is shared by every kind and its output is
/// stamped into `TOKENIZER_PIPELINE_VERSION`, so widening it would re-tokenize
/// mail and conversations too and oblige a full versioned re-index — a large,
/// cross-kind cost to make filenames searchable. Deriving the extra terms in
/// this one doc-shaper is free and affects nothing else.
///
/// So: the basename gives the exact-name match, its **stem** gives the natural
/// one (`albatross`), the full path gives the folder segments, and the
/// **extension** gives search-by-file-type (`jpg`). Repeats of a `FieldKind` are
/// multi-valued in the schema, so these are four values across two fields, not a
/// synthesized string.
fn file_doc_for(file: &IndexableFile) -> IndexedDoc {
    let basename = match file.path.rsplit_once('/') {
        Some((_, name)) if !name.is_empty() => name,
        _ => file.path.as_str(),
    };
    // `rsplit_once` rather than `split_once` so `archive.tar.gz` stems to
    // `archive.tar` and extends `gz`; a leading dot is a hidden file, not an
    // extension, so `.bashrc` keeps its whole name and yields no stem.
    let (stem, ext) = match basename.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (Some(stem), Some(ext)),
        _ => (None, None),
    };
    let mut fields = Vec::with_capacity(4);
    if !basename.is_empty() {
        fields.push(IndexedField {
            kind: FieldKind::Title,
            text: basename.to_string(),
        });
    }
    if let Some(stem) = stem {
        fields.push(IndexedField {
            kind: FieldKind::Title,
            text: stem.to_string(),
        });
    }
    if !file.path.is_empty() {
        fields.push(IndexedField {
            kind: FieldKind::Body,
            text: file.path.clone(),
        });
    }
    if let Some(ext) = ext {
        fields.push(IndexedField {
            kind: FieldKind::Body,
            text: ext.to_string(),
        });
    }
    IndexedDoc {
        kind: ContentKind::File,
        content_id: ContentId(
            file_content_id(file.folder_id, &file.path_hash_hex)
                .as_bytes()
                .to_vec(),
        ),
        // Seconds on the wire, nanos in the index.
        timestamp_ns: file.updated_at_secs.saturating_mul(1_000_000_000),
        // Every file in the corpus is one the user reads through their own sets
        // — including a group-shared set, where attributing the row to whoever
        // last wrote it would need a per-file author the media plane does not
        // carry. Same answer as a card or a post: the index is the user's.
        sender_actor_id: None,
        // Files have no second identity spelling — the secondary carrier is the
        // MAIL kind's.
        secondary_id: None,
        fields,
    }
}

/// Shape one post into its indexed doc. Body only — a post has no title.
fn post_doc_for(post: &IndexablePost) -> IndexedDoc {
    IndexedDoc {
        kind: ContentKind::Post,
        content_id: ContentId(post.post_id.as_bytes().to_vec()),
        timestamp_ns: post.created_at_micros.saturating_mul(1000),
        // The corpus is the user's OWN posts by construction (`fauna.posts.list`
        // is self-scoped), so a sender attribution would only repeat the actor
        // the whole index belongs to.
        sender_actor_id: None,
        // Posts have no second identity spelling — the secondary carrier is
        // the MAIL kind's (`content-index.md` § Where the index is built, the
        // 2026-08-10 carrier ruling).
        secondary_id: None,
        fields: vec![IndexedField {
            kind: FieldKind::Body,
            text: post.text.clone(),
        }],
    }
}

/// Wall-clock nanos for a corpus observation, saturating at 0 before the epoch.
fn observed_now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

/// The index [`ContentKind`] one seam kind stages as.
///
/// Total on purpose — a new [`IndexableKind`] must answer here, which is what
/// makes the seam's two classes explicit at the one place they meet.
fn content_kind_of(kind: IndexableKind) -> ContentKind {
    match kind {
        IndexableKind::Mail => ContentKind::Mail,
        IndexableKind::Conversation => ContentKind::Conversation,
    }
}

/// Shape one ingested message into an indexable doc.
///
/// **No stored fields** — the index holds tokens plus the metadata a hit needs,
/// never a copy of the source text (`content-index.md` § Engine and shape;
/// storing it would roughly double the index and re-introduce content the AEAD
/// nonces stop fauna-sync from de-duplicating). Snippets render from fetched
/// content at query time.
fn doc_for(msg: &IndexableMessage<'_>) -> IndexedDoc {
    let mut fields = Vec::with_capacity(2);
    if let Some(subject) = msg.subject.filter(|s| !s.is_empty()) {
        fields.push(IndexedField {
            kind: FieldKind::Title,
            text: subject.to_string(),
        });
    }
    if !msg.body.is_empty() {
        fields.push(IndexedField {
            kind: FieldKind::Body,
            text: msg.body.to_string(),
        });
    }
    IndexedDoc {
        kind: content_kind_of(msg.kind),
        // The producer-owned RFC `Message-ID` (or the manager's stable
        // synthetic fallback). Stable across devices, which is exactly what
        // lets two independently built segments dedup against each other and
        // what `Index::add_doc`'s upsert keys on.
        content_id: ContentId(msg.message_id.0.as_bytes().to_vec()),
        timestamp_ns: msg.timestamp_ms.saturating_mul(1_000_000),
        sender_actor_id: msg.sender_actor_id.map(|a| a.to_vec()),
        // The nest segment-record id, when the ingest frame held one — the
        // stored secondary identity the MDA's `SEARCH` coverage asks in
        // (`content-index.md` § Where the index is built, 2026-08-10 ruling).
        secondary_id: msg.nest_message_id.map(|b| b.to_vec()),
        fields,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_conversations::message::MessageId;
    use fauna_conversations::thread::ThreadId;
    // Production takes this path from `ClassKey::manifest_path`; the tests name
    // it directly, which is what lets them catch a builder publishing its
    // manifest into the wrong class.
    use fauna_index::mailcal_manifest_path;

    /// Records every published blob, so a test can assert on the exact paths
    /// and re-open the sealed bytes.
    #[derive(Default)]
    struct Recorder {
        published: Mutex<Vec<(String, Vec<u8>)>>,
        fail: Mutex<bool>,
    }

    impl Recorder {
        fn paths(&self) -> Vec<String> {
            self.published
                .lock()
                .unwrap()
                .iter()
                .map(|(p, _)| p.clone())
                .collect()
        }
        fn bytes_at(&self, path: &str) -> Option<Vec<u8>> {
            self.published
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find(|(p, _)| p == path)
                .map(|(_, b)| b.clone())
        }
    }

    #[async_trait::async_trait]
    impl SegmentRail for Recorder {
        /// The live view: the **latest** bytes published at each path, hashed
        /// exactly the way the production rail hashes them. Modelling "last
        /// write wins per path" matters — the manifest is republished on every
        /// flush, so a recorder that listed every historical write would hand
        /// the fold a stale manifest and prove nothing.
        async fn list_entries(&self) -> Result<Vec<RailEntry>, IndexBuildError> {
            let published = self.published.lock().unwrap();
            let mut latest: Vec<(String, Vec<u8>)> = Vec::new();
            for (path, bytes) in published.iter() {
                match latest.iter_mut().find(|(p, _)| p == path) {
                    Some(slot) => slot.1 = bytes.clone(),
                    None => latest.push((path.clone(), bytes.clone())),
                }
            }
            Ok(latest
                .into_iter()
                .map(|(path, bytes)| RailEntry {
                    blob_hash: test_blob_hash(&bytes),
                    size_bytes: bytes.len() as u64,
                    path,
                })
                .collect())
        }

        async fn fetch_blob(&self, blob_hash: &str) -> Result<Vec<u8>, IndexBuildError> {
            self.published
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find(|(_, bytes)| test_blob_hash(bytes) == blob_hash)
                .map(|(_, bytes)| bytes.clone())
                .ok_or_else(|| IndexBuildError::Publish {
                    path: blob_hash.to_string(),
                    reason: "test rail holds no such blob".into(),
                })
        }

        async fn publish(&self, path: &str, bytes: &[u8]) -> Result<(), IndexBuildError> {
            if *self.fail.lock().unwrap() {
                return Err(IndexBuildError::Publish {
                    path: path.to_string(),
                    reason: "test publisher is failing".into(),
                });
            }
            self.published
                .lock()
                .unwrap()
                .push((path.to_string(), bytes.to_vec()));
            Ok(())
        }
    }

    /// The production blob hash, so the fake rail is content-addressed the same
    /// way the real one is.
    fn test_blob_hash(bytes: &[u8]) -> String {
        fauna_core::hex32::encode(&fauna_cbor::Cid::of_raw(bytes).digest())
    }

    /// Flush and assert this builder published **exactly one** segment,
    /// returning it — the shape of every single-kind test here.
    ///
    /// Worth a helper rather than an `unwrap` chain: `flush` reports one entry
    /// per kind it touched, so "one segment" is now an assertion a test makes
    /// rather than a fact the type guarantees, and a multi-kind flush leaking
    /// into a single-kind test should fail loudly here instead of silently
    /// taking `[0]`.
    async fn flush_one(builder: &IndexBuilder) -> SealedSegment {
        let mut segments = builder.flush().await.expect("flush");
        assert_eq!(
            segments.len(),
            1,
            "expected exactly one published segment, got {segments:?}"
        );
        segments.remove(0)
    }

    /// Flush and assert nothing at all was published.
    async fn flush_none(builder: &IndexBuilder) {
        let segments = builder.flush().await.expect("flush");
        assert!(
            segments.is_empty(),
            "expected no published segment, got {segments:?}"
        );
    }

    const MSEK: [u8; 32] = [7u8; 32];

    /// A ceiling small enough that a handful of docs crosses it, so the
    /// bounding tests stay fast. Production is [`MAX_SEGMENT_BYTES`].
    const SMALL_CEILING: usize = 16 * 1024;

    /// `n` bytes of distinct, non-compressible-by-dedup words — real tokens, so
    /// the sealed segment actually grows with the input.
    fn body_of(n: usize) -> String {
        let mut s = String::with_capacity(n + 16);
        let mut i = 0usize;
        while s.len() < n {
            s.push_str(&format!("w{i} "));
            i += 1;
        }
        s
    }

    fn observe(builder: &IndexBuilder, id: &str, subject: &str, body: &str) {
        observe_kind(builder, IndexableKind::Mail, id, subject, body);
    }

    /// A guard seed for a doc that carries no nest record id — what `observe`
    /// stages, and what every rail but SMTP produces.
    fn seeded(id: &[u8]) -> DocIdentity {
        DocIdentity {
            content_id: ContentId(id.to_vec()),
            secondary_id: None,
        }
    }

    fn observe_kind(
        builder: &IndexBuilder,
        kind: IndexableKind,
        id: &str,
        subject: &str,
        body: &str,
    ) {
        let thread = ThreadId("t-1".into());
        let message = MessageId(id.into());
        builder.observe_indexable_message(IndexableMessage {
            kind,
            thread_id: &thread,
            message_id: &message,
            subject: Some(subject),
            body,
            sender_actor_id: None,
            nest_message_id: None,
            timestamp_ms: 1_700_000_000_000,
            is_own: false,
        });
    }

    /// The master-class key a master builder test seals under. Distinct bytes
    /// from [`MSEK`] so a cross-class open failure cannot be a coincidence.
    const MASTER: [u8; 32] = [11u8; 32];

    fn master_builder(rail: Arc<Recorder>) -> IndexBuilder {
        IndexBuilder::master(
            IndexMasterKey::from_bytes(MASTER),
            [ContentKind::Conversation],
            rail,
        )
    }

    /// **The reason this builder holds a kind set at all.** Two kinds of one
    /// class, staged into ONE builder, must produce one segment each on their
    /// own id chains and a SINGLE manifest that lists both — the shape
    /// `content-index.md` § Encryption posture requires ("`manifest.idx` …
    /// covers exactly the master-key kinds").
    ///
    /// The failure this pins is silent, which is why it is asserted on the
    /// published manifest rather than on the call graph: two *separate*
    /// master-class builders would each rewrite `manifest.idx` wholesale from
    /// their own copy, so whichever flushed last would publish a manifest naming
    /// only its own segments. The other kind's blobs would still sit on the rail,
    /// referenced by nothing — unqueryable, and indistinguishable from never
    /// having been built.
    #[tokio::test]
    async fn two_kinds_of_one_class_share_a_manifest_and_keep_separate_chains() {
        let rec = Arc::new(Recorder::default());
        let builder = IndexBuilder::master(
            IndexMasterKey::from_bytes(MASTER),
            [ContentKind::Conversation, ContentKind::Post],
            rec.clone(),
        );

        observe_kind(
            &builder,
            IndexableKind::Conversation,
            "m-1",
            "harbour",
            "a conversation message",
        );
        // The doc-level entry point, which is how every non-message-shaped kind
        // stages (a post has no thread and no subject).
        builder.stage_doc(IndexedDoc {
            kind: ContentKind::Post,
            content_id: ContentId(b"post-1".to_vec()),
            timestamp_ns: 1_700_000_000_000_000_000,
            sender_actor_id: None,
            secondary_id: None,
            fields: vec![IndexedField {
                kind: FieldKind::Body,
                text: "a post about a harbour".into(),
            }],
        });

        let published = builder.flush().await.expect("flush");
        assert_eq!(
            published.iter().map(|s| s.kind).collect::<Vec<_>>(),
            vec![ContentKind::Conversation, ContentKind::Post],
            "one segment per kind, in ContentKind declaration order"
        );

        // Both chains start at 1: the ids are per kind, not per class, so the
        // second kind does not continue the first's numbering.
        let conv_seg = segment_path(ContentKind::Conversation, 1);
        let post_seg = segment_path(ContentKind::Post, 1);
        let paths = rec.paths();
        assert!(
            paths.contains(&conv_seg) && paths.contains(&post_seg),
            "both kinds' first segments published, got {paths:?}"
        );

        // Exactly one manifest write for the whole flush, and it is the master
        // one — not one per kind, and never the mail/calendar file.
        assert_eq!(
            paths
                .iter()
                .filter(|p| *p == &fauna_index::manifest_path())
                .count(),
            1,
            "one manifest write covering both kinds, got {paths:?}"
        );
        assert!(
            !paths.iter().any(|p| p == &mailcal_manifest_path()),
            "a master builder must never touch the mail/calendar manifest"
        );

        // The published manifest — the thing a returning device actually reads —
        // lists both kinds live. This is the assertion that would fail if the two
        // kinds ever went back to being separate builders.
        let key = ClassKey::Master(IndexMasterKey::from_bytes(MASTER));
        let manifest = key
            .open_manifest(
                &rec.bytes_at(&fauna_index::manifest_path())
                    .expect("manifest"),
            )
            .expect("open the master manifest");
        for kind in [ContentKind::Conversation, ContentKind::Post] {
            assert_eq!(
                manifest.kind(kind).map(|km| km.live_segments.clone()),
                Some(vec![1]),
                "{kind:?} must be live in the shared manifest"
            );
        }
    }

    /// A boundary retires exactly the kind it names, even for two kinds sharing
    /// one builder — the per-kind window `content-index.md` § Ingest triggers, v1
    /// requires. A shared flag would let one kind's clean sweep reclassify a
    /// sibling's still-unfinished backlog as trickle, which the lease never
    /// gates.
    #[test]
    fn a_catch_up_boundary_retires_only_its_own_kind() {
        let builder = IndexBuilder::master(
            IndexMasterKey::from_bytes(MASTER),
            [ContentKind::Conversation, ContentKind::Post],
            Arc::new(Recorder::default()),
        );
        assert!(builder.is_catching_up(ContentKind::Conversation));
        assert!(builder.is_catching_up(ContentKind::Post));

        builder.observe_catch_up_complete(IndexableKind::Conversation);

        assert!(!builder.is_catching_up(ContentKind::Conversation));
        assert!(
            builder.is_catching_up(ContentKind::Post),
            "a sibling kind's window must survive another kind's boundary"
        );
    }

    /// A reopen gives back exactly the kind it names, and only one this builder
    /// stages — the per-kind window again, in the other direction. Reopening
    /// every kind would make a stood-down seat withhold a sibling kind's live
    /// trickle until a boundary nobody will ever fire for it.
    #[test]
    fn a_catch_up_reopen_restores_only_its_own_staged_kind() {
        let builder = IndexBuilder::master(
            IndexMasterKey::from_bytes(MASTER),
            [ContentKind::Conversation, ContentKind::Post],
            Arc::new(Recorder::default()),
        );
        builder.observe_catch_up_complete(IndexableKind::Conversation);
        builder.end_catch_up(ContentKind::Post);

        builder.observe_catch_up_reopened(IndexableKind::Conversation);
        assert!(
            builder.is_catching_up(ContentKind::Conversation),
            "the reopened kind is backlog again"
        );
        assert!(
            !builder.is_catching_up(ContentKind::Post),
            "a sibling kind's closed window stays closed"
        );

        builder.observe_catch_up_reopened(IndexableKind::Mail);
        assert!(
            !builder.is_catching_up(ContentKind::Mail),
            "a kind this builder does not stage gains no window"
        );
    }

    /// The other half of `a_master_key_kind_is_never_staged_into_the_mail_slice`:
    /// the master-class builder takes exactly the traffic the mail one drops,
    /// and lands it in the *master* manifest under the *master* key.
    #[tokio::test]
    async fn a_master_builder_stages_conversation_into_the_master_class_slice() {
        let rec = Arc::new(Recorder::default());
        let builder = master_builder(rec.clone());

        observe_kind(
            &builder,
            IndexableKind::Conversation,
            "m-1",
            "harbour",
            "a conversation message",
        );
        assert_eq!(builder.pending_len(), 1, "the master builder's own kind");
        flush_one(&builder).await;

        let paths = rec.paths();
        assert!(
            paths.contains(&fauna_index::manifest_path()),
            "the master class publishes `manifest.idx`, got {paths:?}"
        );
        assert!(
            !paths.iter().any(|p| p == &mailcal_manifest_path()),
            "a master builder must never touch the mail/calendar manifest, got {paths:?}"
        );

        // The segment is a conversation-kind segment, sealed under the master
        // key — and the MSEK-derived mail key cannot open it. That is
        // `content-index.md` § Don't do these (*never wrap a master-key kind
        // under a key a MUA credential reaches*) asserted on the bytes rather
        // than on the call graph.
        let seg = segment_path(ContentKind::Conversation, 1);
        let sealed = rec.bytes_at(&seg).expect("a conversation segment");
        let master = ClassKey::Master(IndexMasterKey::from_bytes(MASTER));
        let index = master
            .open_index(&sealed)
            .expect("open under the master key");
        assert_eq!(
            index
                .query("harbour", &[ContentKind::Conversation], None, 10)
                .expect("query")
                .len(),
            1
        );
        assert!(
            IndexBuilder::mail_key(&MSEK).open_index(&sealed).is_err(),
            "the MSEK-derived mail key must not open a master-class segment"
        );
    }

    /// The constructor's class check fires where the mistake is made, rather
    /// than leaving every later flush to fail `append_segment`'s class guard.
    #[test]
    #[should_panic(expected = "belongs to")]
    fn a_kind_outside_the_keys_class_is_refused_at_construction() {
        let rec = Arc::new(Recorder::default());
        // Mail is a mail/calendar kind; the key seals the master class.
        IndexBuilder::with_manifest(
            ClassKey::Master(IndexMasterKey::from_bytes(MASTER)),
            [ContentKind::Mail],
            IndexManifest::empty(KindClass::Master, TOKENIZER_PIPELINE_VERSION),
            rec,
        );
    }

    #[tokio::test]
    async fn observing_buffers_and_flush_publishes_segment_then_manifest() {
        let rec = Arc::new(Recorder::default());
        let builder = IndexBuilder::mail(&MSEK, rec.clone());

        observe(
            &builder,
            "<a@x>",
            "Lunch plans",
            "shall we meet at the harbour",
        );
        observe(&builder, "<b@x>", "Invoice 42", "payment is due next week");
        assert_eq!(builder.pending_len(), 2, "observing only buffers");
        assert!(rec.paths().is_empty(), "the receive path publishes nothing");

        let sealed = flush_one(&builder).await;
        assert_eq!(sealed.doc_count, 2);
        assert_eq!(
            rec.paths(),
            vec![
                "__index/mail/seg-00000001.idx".to_string(),
                "__index/manifest-mailcal.idx".to_string()
            ],
            "segment publishes BEFORE the manifest that references it"
        );
        assert_eq!(builder.pending_len(), 0, "flush drains the buffer");
    }

    #[tokio::test]
    async fn the_published_segment_is_sealed_and_queryable_only_with_the_derived_key() {
        let rec = Arc::new(Recorder::default());
        let builder = IndexBuilder::mail(&MSEK, rec.clone());
        observe(
            &builder,
            "<a@x>",
            "Lunch plans",
            "shall we meet at the harbour",
        );
        let sealed = flush_one(&builder).await;

        let bytes = rec.bytes_at(&sealed.path).expect("segment published");
        // Sealed: the plaintext the nest would otherwise hold is absent.
        assert!(
            !bytes.windows(7).any(|w| w == b"harbour"),
            "a searchable term appears in the published blob — the segment is \
             NOT sealed, which is the exact defect the nest-side writer was \
             deleted for (content-index.md § Encryption posture)"
        );

        let key =
            IndexSegmentKey::from_bytes(*fauna_mls::wrapped_blob::derive_index_segment_key(&MSEK));
        let index = Index::open_encrypted_mailcal(&bytes, &key).expect("open with the right key");
        let hits = index
            .query("harbour", &[ContentKind::Mail], None, 10)
            .expect("query");
        assert_eq!(hits.len(), 1, "the body is searchable once unsealed");
        assert_eq!(hits[0].content_id, ContentId(b"<a@x>".to_vec()));

        let wrong = IndexSegmentKey::from_bytes(
            *fauna_mls::wrapped_blob::derive_index_segment_key(&[9u8; 32]),
        );
        assert!(
            Index::open_encrypted_mailcal(&bytes, &wrong).is_err(),
            "a different MSEK must not open the segment"
        );
    }

    #[tokio::test]
    async fn the_published_manifest_opens_under_the_same_key_and_lists_the_segment() {
        let rec = Arc::new(Recorder::default());
        let builder = IndexBuilder::mail(&MSEK, rec.clone());
        observe(&builder, "<a@x>", "s", "b");
        builder.flush().await.expect("flush");

        let bytes = rec
            .bytes_at(&mailcal_manifest_path())
            .expect("manifest published");
        let key =
            IndexSegmentKey::from_bytes(*fauna_mls::wrapped_blob::derive_index_segment_key(&MSEK));
        let manifest = IndexManifest::from_sealed_bytes_mailcal(&bytes, &key).expect("open");
        assert_eq!(manifest.class, KindClass::MailCal);
        assert_eq!(manifest.tokenizer_version, TOKENIZER_PIPELINE_VERSION);
        assert_eq!(
            manifest
                .kind(ContentKind::Mail)
                .expect("mail kind present")
                .live_segments,
            vec![1],
            // Segment ids start at 1, not 0 — `KindManifest::empty` seeds
            // `next_seg_id: 1`, so `seg-00000000.idx` is never written.
            "the flushed segment is live in the manifest"
        );
    }

    #[tokio::test]
    async fn a_second_flush_appends_a_new_segment_rather_than_rewriting_the_first() {
        let rec = Arc::new(Recorder::default());
        let builder = IndexBuilder::mail(&MSEK, rec.clone());
        observe(&builder, "<a@x>", "s", "first");
        builder.flush().await.expect("flush 1");
        observe(&builder, "<b@x>", "s", "second");
        let two = flush_one(&builder).await;

        assert_eq!(two.path, "__index/mail/seg-00000002.idx");
        let key =
            IndexSegmentKey::from_bytes(*fauna_mls::wrapped_blob::derive_index_segment_key(&MSEK));
        let manifest = IndexManifest::from_sealed_bytes_mailcal(
            &rec.bytes_at(&mailcal_manifest_path()).unwrap(),
            &key,
        )
        .expect("open");
        assert_eq!(
            manifest.kind(ContentKind::Mail).unwrap().live_segments,
            vec![1, 2],
            "segments are immutable and append-only (spec D4)"
        );
    }

    /// Open every live segment the published manifest names, as a reader does,
    /// and return the content ids a query for `term` finds.
    ///
    /// Deliberately routed through the *manifest* rather than the recorder's
    /// raw publish log: that is what a replica sees, so a fold that tombstoned
    /// the wrong ids, or published a segment the manifest never lists, shows up
    /// here as a changed answer instead of hiding behind bytes still on the
    /// rail.
    fn query_via_manifest(rec: &Recorder, term: &str) -> Vec<ContentId> {
        let key =
            IndexSegmentKey::from_bytes(*fauna_mls::wrapped_blob::derive_index_segment_key(&MSEK));
        let manifest = IndexManifest::from_sealed_bytes_mailcal(
            &rec.bytes_at(&mailcal_manifest_path()).expect("a manifest"),
            &key,
        )
        .expect("open manifest");
        let plaintexts: Vec<Vec<u8>> = manifest
            .kind(ContentKind::Mail)
            .expect("a mail kind")
            .live_segments
            .iter()
            .map(|id| {
                let sealed = rec
                    .bytes_at(&segment_path(ContentKind::Mail, *id))
                    .unwrap_or_else(|| {
                        panic!("the manifest lists live segment {id}, but no blob was published")
                    });
                fauna_index::open_segment_bytes_mailcal(&sealed, &key).expect("unseal")
            })
            .collect();
        let index = Index::open_multi_segment(&plaintexts).expect("open");
        let mut ids: Vec<ContentId> = index
            .query(term, &[ContentKind::Mail], None, 1000)
            .expect("query")
            .into_iter()
            .map(|h| h.content_id)
            .collect();
        ids.sort_by(|a, b| a.0.cmp(&b.0));
        ids.dedup();
        ids
    }

    fn live_segments(rec: &Recorder) -> Vec<u32> {
        let key =
            IndexSegmentKey::from_bytes(*fauna_mls::wrapped_blob::derive_index_segment_key(&MSEK));
        IndexManifest::from_sealed_bytes_mailcal(
            &rec.bytes_at(&mailcal_manifest_path()).expect("a manifest"),
            &key,
        )
        .expect("open manifest")
        .kind(ContentKind::Mail)
        .expect("a mail kind")
        .live_segments
        .clone()
    }

    fn tombstoned_segments(rec: &Recorder) -> Vec<u32> {
        let key =
            IndexSegmentKey::from_bytes(*fauna_mls::wrapped_blob::derive_index_segment_key(&MSEK));
        IndexManifest::from_sealed_bytes_mailcal(
            &rec.bytes_at(&mailcal_manifest_path()).expect("a manifest"),
            &key,
        )
        .expect("open manifest")
        .kind(ContentKind::Mail)
        .expect("a mail kind")
        .tombstoned_segments
        .clone()
    }

    /// Stage one doc and flush, `count` times — one segment per flush, which is
    /// exactly the unbounded chain fold-at-flush exists to collapse.
    async fn flush_n_segments(builder: &IndexBuilder, count: u32) {
        for i in 0..count {
            observe(builder, &format!("<m{i}@x>"), "quarterly", "harbour report");
            flush_one(builder).await;
        }
    }

    #[tokio::test]
    async fn crossing_the_segment_threshold_folds_and_preserves_every_query_result() {
        // The Success clause of the compaction row: a kind crossing the ratified
        // 16-segment threshold ends with fewer live segments, and the query
        // answers exactly what it did before the fold.
        let rec = Arc::new(Recorder::default());
        let builder = IndexBuilder::mail(&MSEK, rec.clone());

        flush_n_segments(&builder, COMPACTION_LIVE_SEGMENT_THRESHOLD as u32 + 1).await;
        let before_live = live_segments(&rec);
        let before_hits = query_via_manifest(&rec, "harbour");
        assert_eq!(
            before_live.len(),
            COMPACTION_LIVE_SEGMENT_THRESHOLD + 1,
            "one segment per flush, so the kind is now past the threshold"
        );
        assert_eq!(before_hits.len(), COMPACTION_LIVE_SEGMENT_THRESHOLD + 1);
        assert!(
            tombstoned_segments(&rec).is_empty(),
            "nothing has folded yet"
        );

        // The flush that folds.
        observe(&builder, "<new@x>", "quarterly", "harbour report");
        flush_one(&builder).await;

        let after_live = live_segments(&rec);
        assert!(
            after_live.len() < before_live.len(),
            "the fold must shrink the live count: {} -> {}",
            before_live.len(),
            after_live.len()
        );
        assert!(
            !tombstoned_segments(&rec).is_empty(),
            "the folded inputs are tombstoned, which is how they leave the query path"
        );

        // Byte-identical results: every id found before is still found, plus the
        // doc this flush staged — nothing dropped, nothing duplicated.
        let after_hits = query_via_manifest(&rec, "harbour");
        let mut expected = before_hits.clone();
        expected.push(ContentId(b"<new@x>".to_vec()));
        expected.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            after_hits, expected,
            "a fold must not change what the index answers"
        );
    }

    #[tokio::test]
    async fn a_fold_never_deletes_a_blob_and_never_leaves_a_dangling_reference() {
        // Tombstone-only is the rule that makes every compaction race
        // non-destructive (`content-index.md` § Where the index is built): the
        // folded blobs stay at rest, and physical reclamation is the deferred
        // distributed-GC question.
        let rec = Arc::new(Recorder::default());
        let builder = IndexBuilder::mail(&MSEK, rec.clone());
        flush_n_segments(&builder, COMPACTION_LIVE_SEGMENT_THRESHOLD as u32 + 1).await;

        let paths_before: Vec<String> = rec
            .published
            .lock()
            .unwrap()
            .iter()
            .map(|(p, _)| p.clone())
            .collect();

        observe(&builder, "<new@x>", "quarterly", "harbour report");
        flush_one(&builder).await;

        // FIRST: prove a fold actually happened. Without this the rest is
        // vacuous — "nothing was deleted" and "no dangling reference" are both
        // trivially true of a build that never compacted, so the whole test
        // would pass with compaction disabled. (It did, until the mutation run
        // caught it.)
        let folded_ids = tombstoned_segments(&rec);
        assert!(
            !folded_ids.is_empty(),
            "no fold was taken, so this test proves nothing about folding"
        );

        // Nothing the fold superseded was removed from the rail.
        let rail_now: Vec<String> = rec
            .published
            .lock()
            .unwrap()
            .iter()
            .map(|(p, _)| p.clone())
            .collect();
        for path in &paths_before {
            assert!(
                rail_now.contains(path),
                "{path} left the rail — a fold must never delete a segment blob"
            );
        }
        for folded in &folded_ids {
            let path = segment_path(ContentKind::Mail, *folded);
            assert!(
                rec.bytes_at(&path).is_some(),
                "tombstoned {path} was deleted — tombstone-only means the bytes stay"
            );
            assert!(
                !live_segments(&rec).contains(folded),
                "segment {folded} is both live and tombstoned"
            );
        }

        // And every id the manifest calls live resolves to a blob that exists —
        // the failure the segment-then-manifest publish order exists to prevent.
        // `query_via_manifest` panics if any live segment is missing.
        let hits = query_via_manifest(&rec, "harbour");
        assert_eq!(hits.len(), COMPACTION_LIVE_SEGMENT_THRESHOLD + 2);
    }

    #[tokio::test]
    async fn a_stale_fold_view_degrades_to_a_no_op_rather_than_corrupting_the_manifest() {
        // The ruled concurrency envelope: the fold plans its inputs off the
        // lock, so another writer can tombstone them first. That must degrade to
        // an un-taken tombstone, never to a manifest naming a blob that does not
        // exist. Simulated by tombstoning the fold's inputs out from under it —
        // which is exactly what a racing builder's manifest write does.
        let rec = Arc::new(Recorder::default());
        let builder = IndexBuilder::mail(&MSEK, rec.clone());
        flush_n_segments(&builder, COMPACTION_LIVE_SEGMENT_THRESHOLD as u32 + 1).await;

        let fold = builder
            .prepare_fold(ContentKind::Mail, None, Vec::new())
            .await
            .expect("prepare")
            .expect("eligible past the threshold");

        // The race: those very segments stop being live before the fold commits.
        {
            let mut manifest = builder.manifest.lock().unwrap();
            for id in &fold.inputs {
                manifest.tombstone_segment(ContentKind::Mail, *id);
            }
        }

        let plan = builder
            .plan_flush(vec![(ContentKind::Mail, None, Some(fold), false)])
            .expect("plan")
            .expect("a fold still publishes its merged segment");
        if let Some(seg) = plan.segments.first() {
            let (path, bytes) = (&seg.path, &seg.bytes);
            builder.publish(path, bytes).await.expect("publish segment");
        }
        builder
            .publish(&builder.key.manifest_path(), &plan.manifest_bytes)
            .await
            .expect("publish manifest");

        // The merged segment is live and its blob exists; the double-tombstone
        // was a no-op. `query_via_manifest` would panic on a dangling reference.
        let hits = query_via_manifest(&rec, "harbour");
        assert_eq!(
            hits.len(),
            COMPACTION_LIVE_SEGMENT_THRESHOLD + 1,
            "every doc survives a lost fold race"
        );
        for folded in tombstoned_segments(&rec) {
            assert!(
                rec.bytes_at(&segment_path(ContentKind::Mail, folded))
                    .is_some(),
                "no blob was deleted by the race"
            );
        }
    }

    #[tokio::test]
    async fn conversation_kind_messages_are_ignored_by_the_mail_builder() {
        let rec = Arc::new(Recorder::default());
        let builder = IndexBuilder::mail(&MSEK, rec.clone());
        let thread = ThreadId("t".into());
        let message = MessageId("<c@x>".into());
        builder.observe_indexable_message(IndexableMessage {
            kind: IndexableKind::Conversation,
            thread_id: &thread,
            message_id: &message,
            subject: None,
            body: "a conversation message",
            sender_actor_id: Some(&[3u8; 32]),
            nest_message_id: None,
            timestamp_ms: 1,
            is_own: false,
        });
        assert_eq!(
            builder.pending_len(),
            0,
            "a master-key kind must never be staged into the mail/calendar \
             slice — this builder holds only the MSEK-derived key"
        );
        flush_none(&builder).await;
    }

    #[tokio::test]
    async fn an_empty_flush_publishes_nothing() {
        let rec = Arc::new(Recorder::default());
        let builder = IndexBuilder::mail(&MSEK, rec.clone());
        flush_none(&builder).await;
        assert!(
            rec.paths().is_empty(),
            "a quiet timer tick must not rewrite the manifest"
        );
    }

    #[tokio::test]
    async fn a_staged_cursor_advance_reaches_the_folder_without_new_docs() {
        let rec = Arc::new(Recorder::default());
        let builder = IndexBuilder::mail(&MSEK, rec.clone());
        builder
            .note_ingest_cursor(ContentKind::Mail, 42)
            .expect("cursor");
        flush_none(&builder).await;
        assert_eq!(
            rec.paths(),
            vec!["__index/manifest-mailcal.idx".to_string()],
            "the backfill's cursor advance is published even with no segment"
        );
        assert_eq!(builder.ingest_cursor(ContentKind::Mail), Some(42));
    }

    #[tokio::test]
    async fn the_ingest_cursor_only_moves_forward() {
        let rec = Arc::new(Recorder::default());
        let builder = IndexBuilder::mail(&MSEK, rec);
        builder
            .note_ingest_cursor(ContentKind::Mail, 42)
            .expect("cursor");
        builder
            .note_ingest_cursor(ContentKind::Mail, 7)
            .expect("cursor");
        assert_eq!(
            builder.ingest_cursor(ContentKind::Mail),
            Some(42),
            "cursors max-merge across builders — a lagging builder must not \
             rewind another's progress"
        );
    }

    /// One segment is one blob on the `__index` rail — there is no
    /// chunk-manifest form — so a flush must never hand the publisher more
    /// bytes than the blob route carries. The ceiling is shrunk here so a few
    /// small docs cross it; the production constant is 8 MiB.
    #[tokio::test]
    async fn an_oversize_batch_halves_instead_of_publishing_one_giant_blob() {
        let rec = Arc::new(Recorder::default());
        let builder = IndexBuilder::mail(&MSEK, rec.clone()).with_segment_ceiling(SMALL_CEILING);
        for i in 0..8 {
            observe(&builder, &format!("<{i}@x>"), "subject", &body_of(2_000));
        }

        let sealed = flush_one(&builder).await;
        assert!(
            sealed.byte_len <= SMALL_CEILING,
            "published {} bytes over a {SMALL_CEILING}-byte ceiling — a segment \
             that outgrows the blob route has nowhere to go (content-index.md \
             § Ingest triggers, v1)",
            sealed.byte_len
        );
        assert!(
            sealed.doc_count < 8,
            "the batch must have been halved to fit, got all {} docs",
            sealed.doc_count
        );
        assert_eq!(
            builder.pending_len(),
            8 - sealed.doc_count,
            "docs that did not fit stay staged for the next flush — a segment \
             chain, never a dropped message"
        );
    }

    /// The pathological case the halving loop must not spin on: a single doc
    /// bigger than a whole segment. It is skipped, not retried forever, and the
    /// rest of the batch still publishes.
    #[tokio::test]
    async fn a_single_doc_larger_than_the_ceiling_is_skipped_not_retried_forever() {
        let rec = Arc::new(Recorder::default());
        let builder = IndexBuilder::mail(&MSEK, rec.clone()).with_segment_ceiling(SMALL_CEILING);
        observe(&builder, "<huge@x>", "subject", &body_of(SMALL_CEILING * 4));

        let sealed = builder.flush().await.expect("flush");
        assert!(
            sealed.is_empty(),
            "nothing publishable came out of the batch"
        );
        assert_eq!(
            builder.pending_len(),
            0,
            "the oversize doc is dropped rather than re-staged — re-staging it \
             would wedge every future flush on the same doc"
        );
    }

    /// A backfill hands the builder far more than one segment's worth at once.
    /// The doc cap is what turns that into a chain.
    #[tokio::test]
    async fn a_batch_over_the_doc_cap_leaves_the_remainder_staged() {
        let rec = Arc::new(Recorder::default());
        let builder = IndexBuilder::mail(&MSEK, rec.clone());
        for i in 0..(MAX_DOCS_PER_SEGMENT + 5) {
            observe(&builder, &format!("<{i}@x>"), "s", "b");
        }

        let sealed = flush_one(&builder).await;
        assert_eq!(sealed.doc_count, MAX_DOCS_PER_SEGMENT);
        assert_eq!(
            builder.pending_len(),
            5,
            "the overflow waits for the next flush"
        );
    }

    /// The defect the 2026-08-03 backfill correction uncovered, in miniature.
    /// The client receive path has no restart-durable cursor, so every launch
    /// re-walks the whole mailbox and the seam fires for all of it; a resumed
    /// builder that re-staged those messages would republish the entire index
    /// as a fresh segment chain on every start.
    #[tokio::test]
    async fn a_resumed_builder_republishes_nothing_when_the_mailbox_is_re_walked() {
        let rec = Arc::new(Recorder::default());
        let first = IndexBuilder::mail(&MSEK, rec.clone());
        observe(&first, "<a@x>", "Lunch plans", "harbour");
        observe(&first, "<b@x>", "Invoice 42", "payment due");
        flush_one(&first).await;
        let after_first = rec.paths().len();

        // Relaunch: same mailbox, same messages, a builder that knows what the
        // published segments already hold.
        let resumed = IndexBuilder::mail(&MSEK, rec.clone());
        resumed.seed_indexed([seeded(b"<a@x>"), seeded(b"<b@x>")]);
        observe(&resumed, "<a@x>", "Lunch plans", "harbour");
        observe(&resumed, "<b@x>", "Invoice 42", "payment due");

        assert_eq!(
            resumed.pending_len(),
            0,
            "a re-walked message that is already indexed must not be staged"
        );
        assert!(
            resumed.flush().await.expect("flush").is_empty(),
            "the relaunch must publish no segment at all"
        );
        assert_eq!(
            rec.paths().len(),
            after_first,
            "the relaunch wrote to the rail — an index that grows by a whole \
             mailbox per launch is unbounded growth on the user's at-rest store"
        );
    }

    /// The guard must not swallow genuinely new mail arriving in the same walk.
    #[tokio::test]
    async fn a_resumed_builder_still_indexes_the_messages_it_has_not_seen() {
        let rec = Arc::new(Recorder::default());
        let resumed = IndexBuilder::mail(&MSEK, rec.clone());
        resumed.seed_indexed([seeded(b"<old@x>")]);

        observe(&resumed, "<old@x>", "s", "already indexed");
        observe(&resumed, "<new@x>", "s", "arrived while we were away");

        let sealed = flush_one(&resumed).await;
        assert_eq!(sealed.doc_count, 1, "exactly the unseen message is sealed");

        let key =
            IndexSegmentKey::from_bytes(*fauna_mls::wrapped_blob::derive_index_segment_key(&MSEK));
        let bytes = rec.bytes_at(&sealed.path).expect("segment published");
        let index = Index::open_encrypted_mailcal(&bytes, &key).expect("open");
        let hits = index
            .query("away", &[ContentKind::Mail], None, 10)
            .expect("query");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].content_id, ContentId(b"<new@x>".to_vec()));
    }

    /// Within one session the guard is self-maintaining: the reconnect backstop
    /// (`ConversationsSession::poll_mail`) re-walks from its own cursor, so the
    /// same message reaches the seam twice without any relaunch.
    #[tokio::test]
    async fn the_same_message_observed_twice_in_one_session_stages_once() {
        let rec = Arc::new(Recorder::default());
        let builder = IndexBuilder::mail(&MSEK, rec);
        observe(&builder, "<a@x>", "s", "b");
        observe(&builder, "<a@x>", "s", "b");
        assert_eq!(builder.pending_len(), 1);
        assert_eq!(builder.indexed_len(), 1);
    }

    /// Observe one message that carries a **nest segment-record id** — the
    /// shape every message the SMTP rail ingests actually has
    /// (`backends::smtp`'s `ingest_inbound_identified` call passes
    /// `Some(rec.message_id)` for both mailboxes).
    /// An `INBOX` record — never the account's own, whatever its headers claim.
    fn observe_inbox(builder: &IndexBuilder, id: &str, subject: &str, body: &str, rec: &[u8]) {
        observe_record(builder, id, subject, body, rec, false);
    }

    /// A `Sent` record — the account's own, which is the only thing that
    /// licenses superseding an already-indexed doc.
    fn observe_sent(builder: &IndexBuilder, id: &str, subject: &str, body: &str, rec: &[u8]) {
        observe_record(builder, id, subject, body, rec, true);
    }

    fn observe_record(
        builder: &IndexBuilder,
        id: &str,
        subject: &str,
        body: &str,
        nest_record: &[u8],
        is_own: bool,
    ) {
        let thread = ThreadId("t-1".into());
        let message = MessageId(id.into());
        builder.observe_indexable_message(IndexableMessage {
            kind: IndexableKind::Mail,
            thread_id: &thread,
            message_id: &message,
            subject: Some(subject),
            body,
            sender_actor_id: None,
            nest_message_id: Some(nest_record),
            timestamp_ms: 1_700_000_000_000,
            is_own,
        });
    }

    /// Relaunch: a **new** builder over what the previous session published —
    /// the test twin of `rail_publisher::resume_mail_builder`.
    ///
    /// Both halves are load-bearing and a helper that did only the second would
    /// make every cross-session test here green for the wrong reason. The
    /// **manifest** is what tells the resumed builder which segments are live,
    /// so without it `append_segment` restarts at id 1 and the flush publishes
    /// *over* the previous session's blob — which looks exactly like a
    /// successful retirement while actually being data loss. The **guard** is
    /// seeded from those segments' identities, content id and nest record id
    /// together.
    fn resume(rec: &Arc<Recorder>) -> IndexBuilder {
        let key =
            IndexSegmentKey::from_bytes(*fauna_mls::wrapped_blob::derive_index_segment_key(&MSEK));
        let manifest = IndexManifest::from_sealed_bytes_mailcal(
            &rec.bytes_at(&mailcal_manifest_path()).expect("a manifest"),
            &key,
        )
        .expect("open manifest");

        let builder = IndexBuilder::with_manifest(
            IndexBuilder::mail_key(&MSEK),
            [ContentKind::Mail],
            manifest.clone(),
            rec.clone(),
        );
        for seg in &manifest
            .kind(ContentKind::Mail)
            .expect("a mail kind")
            .live_segments
        {
            let sealed = rec
                .bytes_at(&segment_path(ContentKind::Mail, *seg))
                .expect("segment published");
            let index = Index::open_encrypted_mailcal(&sealed, &key).expect("open");
            builder.seed_indexed(index.doc_identities().expect("doc identities"));
        }
        builder
    }

    /// The Message-ID collision rule's surviving index trace, **across the
    /// session boundary that is the real case** (`../../docs/goal/ui/conversations.md`
    /// § Receiving into the conversations view → the collision rule; the index
    /// half is ruled in `content-index-ingest.md` § Ingest triggers, v1).
    ///
    /// A forged `INBOX` record squats on a Message-ID this account also sends
    /// under. The first launch holds only the squat and indexes it like any
    /// other mail. The next launch's thread store starts empty, so the mailbox
    /// re-walk re-presents the squat and then the account's own `Sent` copy
    /// arrives and **displaces** it. The index has to follow, or two things are wrong at
    /// once for the user: the genuine body is never tokenized, so nothing finds
    /// the account's own message locally, and the squat's words keep matching
    /// from a segment sealed before the displacement — a hit that resolves
    /// through the live thread store and therefore renders the *genuine*
    /// message, showing a result that does not contain the word searched for.
    ///
    /// **Deliberately across the flush**: a same-session-only version of this
    /// passes without any fix, because `add_doc`'s upsert already collapses two
    /// docs staged into one batch. Once the squat is sealed and published, no
    /// later writer's `delete_term` reaches it
    /// (`fauna-index/tests/cross_segment_supersession.rs`), which is what makes
    /// this the real case.
    #[tokio::test]
    async fn a_sent_copy_displacing_a_squat_retires_the_squats_text_across_a_relaunch() {
        let rec = Arc::new(Recorder::default());

        let first = IndexBuilder::mail(&MSEK, rec.clone());
        observe_inbox(
            &first,
            "<shared@x>",
            "Re: invoice",
            "pay the attacker instead",
            b"inbox-record",
        );
        flush_one(&first).await;

        let resumed = resume(&rec);
        // The re-walk re-presents the squat: already indexed under this very
        // record, so it stages nothing — the guard doing its ordinary job.
        observe_inbox(
            &resumed,
            "<shared@x>",
            "Re: invoice",
            "pay the attacker instead",
            b"inbox-record",
        );
        // The `Sent` copy: same Message-ID, different nest record, different
        // content, and **own**. This is the displacement the collision rule
        // performs, reaching the index.
        observe_sent(
            &resumed,
            "<shared@x>",
            "Re: invoice",
            "here is the signed contract",
            b"sent-record",
        );
        resumed.flush().await.expect("flush");

        assert_eq!(
            query_via_manifest(&rec, "contract"),
            vec![ContentId(b"<shared@x>".to_vec())],
            "the genuine `Sent` body is tokenized — without this the account's \
             own message is unfindable in its own index"
        );
        assert!(
            query_via_manifest(&rec, "attacker").is_empty(),
            "and the displaced squat's text no longer matches"
        );
    }

    /// The rewrite must happen **once**, not once per launch — the reason the
    /// guard keys on the nest record id instead of taking a displacement event
    /// off the seam.
    ///
    /// The squat keeps re-paging from `INBOX` every launch and the thread store
    /// starts empty every launch, so the *displacement itself* re-fires every
    /// launch for as long as the forged record sits in the mailbox. An
    /// event-driven rewrite would therefore republish a segment blob every
    /// launch, for ever. Comparing record ids makes the third launch a plain
    /// match: nothing staged, nothing published.
    #[tokio::test]
    async fn the_rewrite_happens_once_and_a_later_relaunch_republishes_nothing() {
        let rec = Arc::new(Recorder::default());

        let first = IndexBuilder::mail(&MSEK, rec.clone());
        observe_inbox(
            &first,
            "<shared@x>",
            "Re: invoice",
            "pay the attacker",
            b"in",
        );
        flush_one(&first).await;

        let second = resume(&rec);
        observe_inbox(
            &second,
            "<shared@x>",
            "Re: invoice",
            "pay the attacker",
            b"in",
        );
        observe_sent(
            &second,
            "<shared@x>",
            "Re: invoice",
            "signed contract",
            b"sent",
        );
        second.flush().await.expect("the displacement flush");
        let after_rewrite = rec.paths().len();

        // Launch three: the mailbox re-presents both records exactly as before
        // — the squat FIRST, since the store is empty again and `INBOX` pages
        // ahead of `Sent` — and the displacement happens in the thread store
        // all over again. Neither observe may move the index now.
        let third = resume(&rec);
        observe_inbox(
            &third,
            "<shared@x>",
            "Re: invoice",
            "pay the attacker",
            b"in",
        );
        observe_sent(
            &third,
            "<shared@x>",
            "Re: invoice",
            "signed contract",
            b"sent",
        );

        assert_eq!(
            third.pending_len(),
            0,
            "nothing to stage: the squat cannot take the id back (an `INBOX` \
             record never displaces), and the `Sent` copy matches what is \
             indexed, record id included"
        );
        assert!(
            third.flush().await.expect("flush").is_empty(),
            "and nothing to publish"
        );
        assert_eq!(
            rec.paths().len(),
            after_rewrite,
            "a rewrite per launch would be unbounded churn on the user's \
             at-rest store and on every sync replica"
        );
        // Still correct, not merely quiet.
        assert!(query_via_manifest(&rec, "attacker").is_empty());
        assert_eq!(query_via_manifest(&rec, "contract").len(), 1);
    }

    /// The asymmetry in the direction that matters for a forgery: a squat
    /// arriving **after** the account's own copy is indexed must not take the
    /// content id back.
    ///
    /// The conversations view already settles this — "an `INBOX` record never
    /// displaces a held copy, so a squat arriving after the `Sent` copy is
    /// dropped by the dedup like any duplicate". If the index instead
    /// superseded on any content change, a forger who sent a record naming a
    /// Message-ID this account had already sent under would replace the
    /// account's own indexed body with their text, and searches for the real
    /// message would stop finding it. The index must never be the softer
    /// target.
    #[tokio::test]
    async fn a_squat_arriving_after_the_sent_copy_cannot_take_the_id_back() {
        let rec = Arc::new(Recorder::default());

        let first = IndexBuilder::mail(&MSEK, rec.clone());
        observe_sent(
            &first,
            "<shared@x>",
            "Re: invoice",
            "signed contract",
            b"sent",
        );
        flush_one(&first).await;

        let resumed = resume(&rec);
        observe_inbox(
            &resumed,
            "<shared@x>",
            "Re: invoice",
            "pay the attacker",
            b"forged",
        );

        assert_eq!(
            resumed.pending_len(),
            0,
            "the forged record is dropped by the guard, not staged"
        );
        assert!(
            resumed.flush().await.expect("flush").is_empty(),
            "and nothing is published"
        );
        assert!(
            query_via_manifest(&rec, "attacker").is_empty(),
            "the forger's text never entered the index"
        );
        assert_eq!(
            query_via_manifest(&rec, "contract"),
            vec![ContentId(b"<shared@x>".to_vec())],
            "and the account's own message is still findable"
        );
    }

    /// The bystanders in the rewritten segment survive it — the rewrite retires
    /// one doc, not the segment's contents.
    #[tokio::test]
    async fn a_rewrite_keeps_the_other_mail_in_the_segment_it_rewrites() {
        let rec = Arc::new(Recorder::default());

        let first = IndexBuilder::mail(&MSEK, rec.clone());
        observe_inbox(
            &first,
            "<shared@x>",
            "Re: invoice",
            "pay the attacker",
            b"in",
        );
        observe_inbox(&first, "<other@x>", "Lunch", "harbour at noon", b"other");
        flush_one(&first).await;

        let resumed = resume(&rec);
        observe_sent(
            &resumed,
            "<shared@x>",
            "Re: invoice",
            "signed contract",
            b"sent",
        );
        resumed.flush().await.expect("flush");

        assert_eq!(
            query_via_manifest(&rec, "harbour"),
            vec![ContentId(b"<other@x>".to_vec())],
            "the unrelated message in the rewritten segment is still indexed"
        );
        assert!(query_via_manifest(&rec, "attacker").is_empty());
        assert_eq!(query_via_manifest(&rec, "contract").len(), 1);
    }

    /// `seed_indexed` is what `resume_mail_builder` feeds from the segments it
    /// just opened, so the round-trip has to line up: what a builder publishes
    /// is exactly what `Index::content_ids` reports back.
    #[tokio::test]
    async fn published_content_ids_round_trip_through_the_sealed_segment() {
        let rec = Arc::new(Recorder::default());
        let builder = IndexBuilder::mail(&MSEK, rec.clone());
        observe(&builder, "<a@x>", "s", "one");
        observe(&builder, "<b@x>", "s", "two");
        let sealed = flush_one(&builder).await;

        let key =
            IndexSegmentKey::from_bytes(*fauna_mls::wrapped_blob::derive_index_segment_key(&MSEK));
        let index = Index::open_encrypted_mailcal(&rec.bytes_at(&sealed.path).unwrap(), &key)
            .expect("open");
        let mut ids: Vec<Vec<u8>> = index
            .content_ids()
            .expect("content ids")
            .into_iter()
            .map(|c| c.0)
            .collect();
        ids.sort();
        assert_eq!(ids, vec![b"<a@x>".to_vec(), b"<b@x>".to_vec()]);
    }

    #[tokio::test]
    async fn a_publish_failure_surfaces_the_path_and_loses_nothing_recoverable() {
        let rec = Arc::new(Recorder::default());
        let builder = IndexBuilder::mail(&MSEK, rec.clone());
        observe(&builder, "<a@x>", "s", "b");
        *rec.fail.lock().unwrap() = true;

        let err = builder.flush().await.expect_err("publish fails");
        match err {
            IndexBuildError::Publish { path, .. } => {
                assert_eq!(path, "__index/mail/seg-00000001.idx")
            }
            other => panic!("expected a publish error, got {other:?}"),
        }
        // The advisory cursor never advanced, so the backfill re-ingests this
        // content — nothing a user cannot recreate is lost.
        assert_eq!(builder.ingest_cursor(ContentKind::Mail), None);
    }

    // ── The advisory `index` lease's catch-up gate ────────────────────────────
    //
    // `content-index.md` § Where the index is built → *The builder and the
    // advisory task lease*: a stand-down binds only build work whose queue
    // **re-presents** (the launch catch-up walk), never the receive-path
    // trickle. Every test below pins one side of that split, and the split is
    // what makes them mean anything — a suite that only checked "a closed gate
    // stages nothing" would happily accept a builder that silently loses live
    // mail on a stood-down seat.

    /// A closed gate + a live builder = the whole point of the ruling. The
    /// trickle is **never** gated: a doc that flows past the seam after the
    /// catch-up boundary is seen once per session, so withholding it would leave
    /// mail the user can see in their inbox permanently unsearchable on this
    /// seat until some later relaunch.
    #[test]
    fn a_stood_down_seat_still_stages_a_live_arrival() {
        let rec = Arc::new(Recorder::default());
        let gate = Arc::new(AtomicBool::new(false));
        let builder = IndexBuilder::mail(&MSEK, rec).with_lease_gate(Arc::clone(&gate));

        // The session's first mail sweep finished: everything from here is
        // trickle.
        builder.end_catch_up(ContentKind::Mail);
        observe(&builder, "<live@x>", "arrived now", "body");

        assert_eq!(
            builder.pending_len(),
            1,
            "a stood-down seat must still index mail that arrives while it is standing by"
        );
    }

    /// The half a stand-down *may* bind: the launch backlog. It re-presents on
    /// every launch (the receive loop re-pages from UID 0 and the stage-time
    /// guard recomputes the missing set), so yielding it is retried by
    /// construction rather than lost.
    #[test]
    fn a_stood_down_seat_skips_the_catch_up_backlog() {
        let rec = Arc::new(Recorder::default());
        let gate = Arc::new(AtomicBool::new(false));
        let builder = IndexBuilder::mail(&MSEK, rec).with_lease_gate(gate);

        assert!(
            builder.is_catching_up(ContentKind::Mail),
            "a fresh builder starts in catch-up"
        );
        observe(&builder, "<backlog@x>", "old mail", "body");

        assert_eq!(
            builder.pending_len(),
            0,
            "the lease holder is republishing this backlog; this seat must not duplicate it"
        );
    }

    /// The holder does all of it — the gate withholds nothing from the seat
    /// that won the lease.
    #[test]
    fn the_lease_holder_stages_backlog_and_trickle_alike() {
        let rec = Arc::new(Recorder::default());
        let gate = Arc::new(AtomicBool::new(true));
        let builder = IndexBuilder::mail(&MSEK, rec).with_lease_gate(gate);

        observe(&builder, "<backlog@x>", "old mail", "body");
        builder.end_catch_up(ContentKind::Mail);
        observe(&builder, "<live@x>", "arrived now", "body");

        assert_eq!(builder.pending_len(), 2, "the holder stages both shapes");
    }

    /// The draft corpus is stream-shaped: `DraftStore` offers it once per
    /// change and once at observer registration, and nothing re-presents it —
    /// so a closed gate must not withhold it (`has_catch_up_window`). Pinned on
    /// the registration re-offer's own timing: a fresh builder whose gate has
    /// not opened yet, because the lease loop's first observe is still in
    /// flight.
    #[test]
    fn a_stood_down_seat_still_stages_the_draft_corpus() {
        let rec = Arc::new(Recorder::default());
        let gate = Arc::new(AtomicBool::new(false));
        let builder = IndexBuilder::master(
            crate::IndexMasterKey::from_bytes([9u8; 32]),
            [ContentKind::Draft],
            rec,
        )
        .with_lease_gate(gate);

        assert!(
            !builder.is_catching_up(ContentKind::Draft),
            "a draft corpus never re-presents, so it has no catch-up window to withhold"
        );
        builder.observe_draft_corpus(&[IndexableDraft {
            content_id: "thread-a".into(),
            thread_id: Some(ThreadId("thread-a".into())),
            subject: None,
            body: "flamingo migration notes".into(),
        }]);

        assert_eq!(
            builder.pending_len(),
            1,
            "a stood-down seat must still stage the draft corpus it was handed — \
             nothing will ever hand it over again"
        );
    }

    /// **The compatibility pin.** A build that wires no lease behaves exactly as
    /// it did before the lease existed — including during catch-up, where an
    /// attached-but-closed gate would withhold. This is what lets the heartbeat
    /// be additive across a mixed fleet (a seat with no lease never heartbeats and is
    /// simply uncoordinated), and it is why `lease_gate` is an `Option` rather
    /// than a gate that defaults closed.
    #[test]
    fn an_unattached_gate_keeps_the_uncoordinated_behaviour() {
        let rec = Arc::new(Recorder::default());
        let builder = IndexBuilder::mail(&MSEK, rec);

        assert!(builder.is_catching_up(ContentKind::Mail));
        observe(&builder, "<backlog@x>", "old mail", "body");

        assert_eq!(
            builder.pending_len(),
            1,
            "no lease attached ⇒ no stand-down; the pre-lease behaviour must survive byte-for-byte"
        );
    }

    /// **The trap this ordering exists for.** The lease check must precede the
    /// re-index guard's insert, or a withheld doc would be remembered as
    /// *indexed* — and then this session's own mid-walk lease acquisition could
    /// never pick it up, turning an advisory yield into a permanent per-session
    /// loss. "Retried by construction" has to hold within the session too, not
    /// only across launches.
    #[test]
    fn a_withheld_backlog_doc_is_not_recorded_as_indexed() {
        let rec = Arc::new(Recorder::default());
        let gate = Arc::new(AtomicBool::new(false));
        let builder = IndexBuilder::mail(&MSEK, rec).with_lease_gate(Arc::clone(&gate));

        observe(&builder, "<contested@x>", "old mail", "body");
        assert_eq!(builder.pending_len(), 0, "withheld while standing by");
        assert_eq!(
            builder.indexed_len(),
            0,
            "a withheld doc must leave no trace in the re-index guard"
        );

        // The peer holding the lease went away and this seat took over, still
        // inside its catch-up walk.
        gate.store(true, Ordering::Release);
        observe(&builder, "<contested@x>", "old mail", "body");

        assert_eq!(
            builder.pending_len(),
            1,
            "the doc the yield skipped must still be stageable once this seat holds the lease"
        );
    }

    // ── The flush error path ────────────────────────────────
    //
    // The publish ORDER already made a crash safe: segments first, manifest
    // last, so a crash leaves unreferenced blobs rather than a manifest naming
    // one that does not exist. The **error** path did not hold the same line —
    // `plan_flush` mutated the shared manifest before publishing anything, and a
    // publish failure left those mutations in place for the next successful
    // flush to publish durably. These pin the guarantee at the same strength for
    // both, plus the retry the failure log promises.
    //
    // The assertion is the general invariant — *every live segment the published
    // manifest names is on the rail* — rather than a hand-picked id, so it holds
    // for kinds and flush shapes nobody wrote a case for.

    /// Every segment the published manifest lists live, for every kind, must be
    /// a blob the rail actually holds. This is the property both probe shapes
    /// violate, and it is what "the error path is as safe as the crash path"
    /// means operationally.
    fn assert_manifest_is_honored_by_the_rail(rec: &Recorder, key: &IndexMasterKey) {
        let manifest = IndexManifest::from_sealed_bytes(
            &rec.bytes_at(&fauna_index::manifest_path())
                .expect("a manifest"),
            key,
        )
        .expect("open manifest");
        let paths = rec.paths();
        for kind in KindClass::Master.kinds() {
            let Some(km) = manifest.kind(kind) else {
                continue;
            };
            for id in &km.live_segments {
                let path = segment_path(kind, *id);
                assert!(
                    paths.contains(&path),
                    "the published manifest lists {kind:?} segment {id} as live, \
                     but `{path}` was never published — a reader would treat the \
                     index as corrupt"
                );
            }
        }
    }

    /// Probe A, the append shape: one transient publish failure must leave no
    /// trace in the manifest the *next* flush publishes.
    ///
    /// Before the fix the failed flush's `append_segment` had already burned
    /// segment id 1 on the live builder, so the next success published
    /// `live: [1, 2]` with only seg-2 on the rail — and nothing ever retires an
    /// append segment, so that phantom is permanent.
    #[tokio::test]
    async fn a_failed_append_flush_leaves_no_phantom_in_the_next_manifest() {
        let rec = Arc::new(Recorder::default());
        let builder = master_builder(Arc::clone(&rec));

        observe_kind(
            &builder,
            IndexableKind::Conversation,
            "m-1",
            "s",
            "flamingo",
        );
        *rec.fail.lock().unwrap() = true;
        builder.flush().await.expect_err("the publish fails");

        *rec.fail.lock().unwrap() = false;
        builder.flush().await.expect("the retry succeeds");

        assert_manifest_is_honored_by_the_rail(&rec, &IndexMasterKey::from_bytes(MASTER));
    }

    /// Probe B, the snapshot shape — the severe one. A failed corpus publish
    /// used to leave the supersession tombstones applied, so a **sibling kind's**
    /// flush published `live: [2] tombstoned: [1]` with only seg-1 on the rail:
    /// the user's only real draft corpus retired in favour of a blob that never
    /// existed, and drafts vanishing from search on every seat.
    ///
    /// A hostile nest can drive exactly this by accepting manifest writes while
    /// failing blob PUTs — a durable denial where withholding is only transient.
    #[tokio::test]
    async fn a_failed_corpus_flush_never_retires_the_corpus_that_is_still_live() {
        let rec = Arc::new(Recorder::default());
        let builder = IndexBuilder::master(
            IndexMasterKey::from_bytes(MASTER),
            [ContentKind::Draft, ContentKind::Conversation],
            rec.clone(),
        );

        builder.observe_draft_corpus(&[draft("thread-a", "flamingo migration notes")]);
        builder.flush().await.expect("the first corpus publishes");
        assert_eq!(master_hits(&rec, "flamingo").len(), 1);

        // The user edits the draft; publishing the new corpus fails.
        builder.observe_draft_corpus(&[draft("thread-a", "penguin migration notes")]);
        *rec.fail.lock().unwrap() = true;
        builder.flush().await.expect_err("the corpus publish fails");

        // An unrelated kind flushes — the innocent-looking step that used to
        // publish the poisoned manifest.
        *rec.fail.lock().unwrap() = false;
        observe_kind(
            &builder,
            IndexableKind::Conversation,
            "m-1",
            "s",
            "unrelated",
        );
        builder.flush().await.expect("the sibling flush succeeds");

        assert_manifest_is_honored_by_the_rail(&rec, &IndexMasterKey::from_bytes(MASTER));
        assert_eq!(
            master_hits(&rec, "penguin").len(),
            1,
            "the restored corpus is published by the retry, so the edit is findable"
        );
        assert!(
            master_hits(&rec, "flamingo").is_empty(),
            "and the superseded text is gone — the supersession happened once, \
             properly, rather than being half-applied by the failure"
        );
    }

    /// The retry the failure log promises has to be a real one: a failed flush's
    /// docs must still be staged afterwards.
    ///
    /// `take_batch` drains `pending` before anything is sealed and the re-index
    /// guard keeps their ids, so before the fix those docs were unreachable for
    /// the rest of the session — an append kind's until relaunch, a snapshot
    /// kind's until the producer happened to offer the corpus again.
    #[tokio::test]
    async fn a_failed_flush_restages_its_batch_so_the_next_one_carries_it() {
        let rec = Arc::new(Recorder::default());
        let builder = master_builder(Arc::clone(&rec));

        observe_kind(
            &builder,
            IndexableKind::Conversation,
            "m-1",
            "s",
            "flamingo",
        );
        *rec.fail.lock().unwrap() = true;
        builder.flush().await.expect_err("the publish fails");

        // Nothing new is observed — the next flush must carry the failed batch
        // on its own, which is exactly what "retrying on the next trigger" says.
        *rec.fail.lock().unwrap() = false;
        let published = builder.flush().await.expect("the retry succeeds");
        assert_eq!(
            published.iter().map(|s| s.doc_count).sum::<usize>(),
            1,
            "the doc from the failed flush is published by the retry"
        );
    }

    /// The direct staging doors admit only this builder's kinds, matching the
    /// observer doors.
    ///
    /// A same-class doc outside the kind set used to seal, append and publish
    /// normally while `catching_up` never held its kind — so the lease could not
    /// gate it, and no reader would answer for it either: indexed, replicated
    /// and unsearchable, from one mis-wired producer.
    #[tokio::test]
    async fn the_direct_doors_drop_a_kind_this_builder_does_not_stage() {
        let rec = Arc::new(Recorder::default());
        // Master class, but Draft is deliberately not in the set.
        let builder = master_builder(Arc::clone(&rec));

        builder.stage_doc(draft_doc_for(&draft("thread-a", "flamingo")));
        builder.stage_snapshot(
            ContentKind::Draft,
            vec![draft_doc_for(&draft("thread-b", "penguin"))],
        );

        assert!(
            builder.flush().await.expect("flush").is_empty(),
            "a kind outside the builder's set must not reach a segment at all"
        );
    }

    // ── Snapshot kinds (drafts) ───────────────────────────────────────────────

    fn drafts_builder(rail: Arc<Recorder>) -> IndexBuilder {
        IndexBuilder::master(
            IndexMasterKey::from_bytes(MASTER),
            [ContentKind::Draft],
            rail,
        )
    }

    fn draft(id: &str, body: &str) -> IndexableDraft {
        IndexableDraft {
            content_id: id.to_string(),
            thread_id: Some(ThreadId(id.to_string())),
            subject: None,
            body: body.to_string(),
        }
    }

    /// The master-class live view: every content id a query for `term` finds
    /// across the segments the published manifest still lists as **live**.
    ///
    /// Reading through the manifest rather than over every blob ever published
    /// is the whole point — a tombstoned segment's bytes stay at rest, so a
    /// helper that opened them all would report the stale copies this design
    /// exists to retire and no supersession test could ever fail.
    fn master_hits(rec: &Recorder, term: &str) -> Vec<ContentId> {
        master_kind_hits(rec, ContentKind::Draft, term)
    }

    /// [`master_hits`] for an arbitrary master kind — same live-view reasoning.
    fn master_kind_hits(rec: &Recorder, kind: ContentKind, term: &str) -> Vec<ContentId> {
        let key = IndexMasterKey::from_bytes(MASTER);
        let manifest = IndexManifest::from_sealed_bytes(
            &rec.bytes_at(&fauna_index::manifest_path())
                .expect("a manifest"),
            &key,
        )
        .expect("open manifest");
        let Some(km) = manifest.kind(kind) else {
            return Vec::new();
        };
        let plaintexts: Vec<Vec<u8>> = km
            .live_segments
            .iter()
            .map(|id| {
                let sealed = rec.bytes_at(&segment_path(kind, *id)).unwrap_or_else(|| {
                    panic!("the manifest lists live segment {id}, but no blob was published")
                });
                fauna_index::open_segment_bytes(&sealed, &key).expect("unseal")
            })
            .collect();
        let index = Index::open_multi_segment(&plaintexts).expect("open");
        let mut ids: Vec<ContentId> = index
            .query(term, &[kind], None, 1000)
            .expect("query")
            .into_iter()
            .map(|h| h.content_id)
            .collect();
        ids.sort_by(|a, b| a.0.cmp(&b.0));
        ids
    }

    /// **The drafts arm's whole reason for existing**, and the clause that
    /// separates a mutable kind from mail: after an edit, the new text is
    /// findable and the old text is not.
    ///
    /// Impossible to satisfy by re-staging, however the re-index guard is keyed:
    /// the first version is sealed into segment 1, and no later `add_doc` can
    /// delete out of it (`fauna-index/tests/cross_segment_supersession.rs`). It
    /// holds here only because the flush *retires* segment 1.
    #[tokio::test]
    async fn an_edited_draft_stops_matching_its_old_text() {
        let rec = Arc::new(Recorder::default());
        let builder = drafts_builder(Arc::clone(&rec));

        builder.observe_draft_corpus(&[draft("thread-a", "flamingo migration notes")]);
        builder.flush().await.expect("first flush");
        assert_eq!(
            master_hits(&rec, "flamingo").len(),
            1,
            "the draft is findable by its original text"
        );

        // The user rewrites it.
        builder.observe_draft_corpus(&[draft("thread-a", "penguin migration notes")]);
        builder.flush().await.expect("second flush");

        assert_eq!(
            master_hits(&rec, "penguin").len(),
            1,
            "the new text is findable"
        );
        assert!(
            master_hits(&rec, "flamingo").is_empty(),
            "the text the user deleted must stop matching — the segment holding \
             it is no longer live"
        );
    }

    /// A discarded draft leaves the corpus entirely. There is no doc to stage
    /// for this, which is why an empty corpus has to be an event rather than a
    /// no-op flush.
    #[tokio::test]
    async fn a_discarded_draft_stops_matching_at_all() {
        let rec = Arc::new(Recorder::default());
        let builder = drafts_builder(Arc::clone(&rec));

        builder.observe_draft_corpus(&[draft("thread-a", "flamingo migration notes")]);
        builder.flush().await.expect("first flush");
        assert_eq!(master_hits(&rec, "flamingo").len(), 1);

        // The user discards it: the corpus is now empty.
        builder.observe_draft_corpus(&[]);
        builder.flush().await.expect("second flush");

        assert!(
            master_hits(&rec, "flamingo").is_empty(),
            "a discarded draft must leave the index; nothing else can express a \
             deletion in an append-only segment chain"
        );
    }

    /// One draft's edit must not take its siblings down with it: the corpus is
    /// republished whole, so every *other* draft is still there afterwards.
    ///
    /// This is the failure mode a "publish only what changed" optimization would
    /// introduce, and it would be invisible until a user searched for the
    /// untouched draft.
    #[tokio::test]
    async fn superseding_one_draft_keeps_the_rest_of_the_corpus() {
        let rec = Arc::new(Recorder::default());
        let builder = drafts_builder(Arc::clone(&rec));

        builder.observe_draft_corpus(&[
            draft("thread-a", "flamingo notes"),
            draft("thread-b", "walrus notes"),
        ]);
        builder.flush().await.expect("first flush");

        builder.observe_draft_corpus(&[
            draft("thread-a", "penguin notes"),
            draft("thread-b", "walrus notes"),
        ]);
        builder.flush().await.expect("second flush");

        assert_eq!(
            master_hits(&rec, "walrus"),
            vec![ContentId(b"thread-b".to_vec())],
            "the untouched draft survives its sibling's supersession"
        );
        assert_eq!(master_hits(&rec, "penguin").len(), 1, "the edit landed");
        assert!(
            master_hits(&rec, "flamingo").is_empty(),
            "the old text is gone"
        );
    }

    /// The rewrite bounds what is *live*, never what is *at rest*: retirement is
    /// tombstone-only, exactly as compaction is (`content-index.md` § Where the
    /// index is built — *Tombstone-only*).
    #[tokio::test]
    async fn supersession_tombstones_rather_than_deleting() {
        let rec = Arc::new(Recorder::default());
        let builder = drafts_builder(Arc::clone(&rec));

        builder.observe_draft_corpus(&[draft("thread-a", "flamingo notes")]);
        builder.flush().await.expect("first flush");
        let first_seg = segment_path(ContentKind::Draft, 1);
        assert!(rec.paths().contains(&first_seg), "segment 1 was published");

        builder.observe_draft_corpus(&[draft("thread-a", "penguin notes")]);
        builder.flush().await.expect("second flush");

        let key = IndexMasterKey::from_bytes(MASTER);
        let manifest = IndexManifest::from_sealed_bytes(
            &rec.bytes_at(&fauna_index::manifest_path()).unwrap(),
            &key,
        )
        .expect("open manifest");
        let km = manifest.kind(ContentKind::Draft).expect("a draft kind");
        assert_eq!(km.live_segments, vec![2], "only the new corpus is live");
        assert_eq!(km.tombstoned_segments, vec![1], "the old one is tombstoned");
        assert!(
            rec.bytes_at(&first_seg).is_some(),
            "the retired blob stays at rest — a compactor never deletes"
        );
    }

    /// A builder that does not stage drafts ignores the corpus entirely, the
    /// same way it ignores another class's message.
    #[tokio::test]
    async fn a_non_drafts_builder_ignores_the_corpus() {
        let rec = Arc::new(Recorder::default());
        let builder = master_builder(Arc::clone(&rec));

        builder.observe_draft_corpus(&[draft("thread-a", "flamingo notes")]);
        assert_eq!(builder.pending_len(), 0, "not this builder's kind");
        assert!(
            builder.flush().await.expect("flush").is_empty(),
            "nothing to publish"
        );
    }

    /// The re-index guard is an append-kind mechanism and must not apply here:
    /// re-staging the same draft twice has to reach the builder both times, or
    /// the second version would never be published at all.
    #[test]
    fn a_snapshot_kind_is_not_held_back_by_the_re_index_guard() {
        let rec = Arc::new(Recorder::default());
        let builder = drafts_builder(rec);

        builder.observe_draft_corpus(&[draft("thread-a", "first")]);
        builder.observe_draft_corpus(&[draft("thread-a", "second")]);

        assert_eq!(
            builder.pending_len(),
            1,
            "the corpus replaces its predecessor rather than accumulating"
        );
        assert_eq!(
            builder.indexed_len(),
            0,
            "a snapshot kind never populates the guard — an entry there would \
             pin its first version forever"
        );
    }

    // ── The contacts arm (third ingest class) ────────────────────────────────

    fn contacts_builder(rail: Arc<Recorder>) -> IndexBuilder {
        IndexBuilder::master(
            IndexMasterKey::from_bytes(MASTER),
            [ContentKind::Contact],
            rail,
        )
    }

    fn contact(uid_hash: &str, text: &str) -> IndexableContact {
        IndexableContact {
            uid_hash: uid_hash.to_string(),
            display_name: None,
            text: text.to_string(),
        }
    }

    fn contact_hits(rec: &Recorder, term: &str) -> Vec<ContentId> {
        master_kind_hits(rec, ContentKind::Contact, term)
    }

    /// Contacts are a snapshot kind, so the clause that separates them from an
    /// append kind holds: after an in-place vCard edit, the new text is
    /// findable and the old text is not.
    ///
    /// This is the whole reason `is_snapshot_kind(Contact)` is `true`. An
    /// append arm could not satisfy it however its stage-time guard were keyed
    /// — the first version is sealed into a segment no later `add_doc` can
    /// delete out of (`fauna-index/tests/cross_segment_supersession.rs`).
    #[tokio::test]
    async fn an_edited_card_stops_matching_its_old_text() {
        let rec = Arc::new(Recorder::default());
        let builder = contacts_builder(Arc::clone(&rec));

        builder.stage_contact_corpus(&[contact("aa", "Alice Flamingo")]);
        builder.flush().await.expect("first flush");
        assert_eq!(contact_hits(&rec, "flamingo").len(), 1);

        // The card is edited through a CardDAV MUA; the walk re-reads the book.
        builder.stage_contact_corpus(&[contact("aa", "Alice Penguin")]);
        builder.flush().await.expect("second flush");

        assert_eq!(
            contact_hits(&rec, "penguin").len(),
            1,
            "the new text is findable"
        );
        assert!(
            contact_hits(&rec, "flamingo").is_empty(),
            "the card's old text must stop matching — the segment holding it is \
             no longer live"
        );
    }

    /// **The per-KIND corpus trap, pinned** (`content-index.md` § Ingest
    /// triggers, v1 — the contacts ruling's last sub-bullet).
    ///
    /// `stage_snapshot`'s supersede arm tombstones every live segment *of the
    /// kind*, so a walk that staged book-by-book would have each book retire
    /// the one before it. This asserts the shape that makes the whole-corpus
    /// call mandatory: two books staged together are both findable, and a
    /// second staging carrying only one book's cards drops the other's — which
    /// is exactly what a per-book walk would do on every run.
    #[tokio::test]
    async fn a_contact_corpus_spans_every_book_and_staging_one_retires_the_rest() {
        let rec = Arc::new(Recorder::default());
        let builder = contacts_builder(Arc::clone(&rec));

        // One call, both books — what the walk must do.
        builder.stage_contact_corpus(&[
            contact("aa", "Alice Flamingo"),
            contact("bb", "Bob Penguin"),
        ]);
        builder.flush().await.expect("flush");
        assert_eq!(contact_hits(&rec, "flamingo").len(), 1, "book A's card");
        assert_eq!(contact_hits(&rec, "penguin").len(), 1, "book B's card");

        // The failure mode: a corpus carrying only one book.
        builder.stage_contact_corpus(&[contact("aa", "Alice Flamingo")]);
        builder.flush().await.expect("flush");
        assert_eq!(contact_hits(&rec, "flamingo").len(), 1);
        assert!(
            contact_hits(&rec, "penguin").is_empty(),
            "a partial corpus retires the books it omits — which is why the walk \
             assembles every book before it stages, and why a failed book read \
             must abandon the walk rather than stage what it got"
        );
    }

    /// A card deleted from every book leaves the corpus entirely — the empty
    /// corpus is an event, not a quiet flush.
    #[tokio::test]
    async fn deleting_the_last_card_empties_the_contact_corpus() {
        let rec = Arc::new(Recorder::default());
        let builder = contacts_builder(Arc::clone(&rec));

        builder.stage_contact_corpus(&[contact("aa", "Alice Flamingo")]);
        builder.flush().await.expect("first flush");
        assert_eq!(contact_hits(&rec, "flamingo").len(), 1);

        builder.stage_contact_corpus(&[]);
        builder.flush().await.expect("second flush");
        assert!(
            contact_hits(&rec, "flamingo").is_empty(),
            "the user's last card left the book, so it must leave the index"
        );
    }

    /// The corpus marker round-trips through the published manifest, which is
    /// what lets the **next launch's** walk recognize an unchanged address book
    /// without re-reading it.
    ///
    /// `None` before anything is staged is the load-bearing half: a walk must
    /// read that as *unknown* and go read the corpus, never as *unchanged*.
    #[tokio::test]
    async fn the_corpus_marker_survives_a_flush_and_starts_unset() {
        let rec = Arc::new(Recorder::default());
        let builder = contacts_builder(Arc::clone(&rec));
        assert_eq!(
            builder.corpus_marker(ContentKind::Contact),
            None,
            "a fresh builder knows of no staged corpus"
        );

        builder.stage_contact_corpus(&[contact("aa", "Alice Flamingo")]);
        builder
            .note_corpus_marker(ContentKind::Contact, vec![7u8; 32])
            .expect("note the marker");
        builder.flush().await.expect("flush");

        let manifest = IndexManifest::from_sealed_bytes(
            &rec.bytes_at(&fauna_index::manifest_path())
                .expect("a manifest"),
            &IndexMasterKey::from_bytes(MASTER),
        )
        .expect("open manifest");
        assert_eq!(
            manifest.corpus_marker(ContentKind::Contact),
            Some([7u8; 32].as_slice()),
            "the marker rides the published manifest, or every launch re-reads \
             and republishes an identical corpus onto the tombstone-only rail"
        );
    }

    /// A builder that does not stage contacts drops the corpus rather than
    /// sealing it under the wrong class's key — the twin of the drafts case.
    #[tokio::test]
    async fn a_non_contacts_builder_ignores_the_corpus() {
        let rec = Arc::new(Recorder::default());
        let builder = drafts_builder(Arc::clone(&rec));

        builder.stage_contact_corpus(&[contact("aa", "Alice Flamingo")]);

        assert_eq!(
            builder.pending_len(),
            0,
            "a builder stages only the kinds of its own set"
        );
    }

    // ── the posts doors ────────────────────────────────────────────────────

    fn posts_builder(rail: Arc<Recorder>) -> IndexBuilder {
        IndexBuilder::master(
            IndexMasterKey::from_bytes(MASTER),
            [ContentKind::Post],
            rail,
        )
    }

    fn post(post_id: &str, created_at_micros: i64, text: &str) -> IndexablePost {
        IndexablePost {
            post_id: post_id.to_string(),
            created_at_micros,
            text: text.to_string(),
        }
    }

    fn post_hits(rec: &Recorder, term: &str) -> Vec<ContentId> {
        master_kind_hits(rec, ContentKind::Post, term)
    }

    /// The append shape, end to end: the walk stages what it has not indexed,
    /// and a re-walk of the same rows publishes **nothing** — the stage-time
    /// content-id guard transferring to posts as ruled (`content-index.md`
    /// § Ingest triggers, v1: a post id names an immutable content-addressed
    /// body, so re-staging a known id could only duplicate it).
    #[tokio::test]
    async fn a_posts_walk_stages_unseen_ids_and_a_rewalk_republishes_nothing() {
        let rec = Arc::new(Recorder::default());
        let builder = posts_builder(Arc::clone(&rec));

        assert!(builder.stage_posts_walk(&[
            post("aa", 1_000, "the albatross crossed the meridian"),
            post("bb", 2_000, "provisions for the long haul"),
        ]));
        builder.flush().await.expect("first flush");
        assert_eq!(post_hits(&rec, "albatross").len(), 1);
        assert_eq!(post_hits(&rec, "provisions").len(), 1);

        // The next sweep's walk re-encounters the same enumeration rows.
        assert!(
            builder.stage_posts_walk(&[post("aa", 1_000, "the albatross crossed the meridian")]),
            "an all-guarded pass is still an admitted pass — the marker may advance"
        );
        flush_none(&builder).await;
    }

    /// A stood-down seat's walk is withheld — and *reports* it, which is what
    /// keeps the walk's corpus marker honest (a marker noted over a withheld
    /// stage would suppress the corpus until the next real change; the
    /// `DirectStager` door pins that half in `lifecycle.rs`).
    #[tokio::test]
    async fn a_stood_down_seats_posts_walk_is_withheld_and_reports_it() {
        let rec = Arc::new(Recorder::default());
        let gate = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let builder = posts_builder(Arc::clone(&rec)).with_lease_gate(Arc::clone(&gate));

        assert!(
            !builder.stage_posts_walk(&[post("aa", 1_000, "the albatross")]),
            "a closed gate withholds the pass-shaped walk"
        );
        flush_none(&builder).await;

        gate.store(true, std::sync::atomic::Ordering::Release);
        assert!(
            builder.stage_posts_walk(&[post("aa", 1_000, "the albatross")]),
            "gaining the lease admits the same pass"
        );
        builder.flush().await.expect("flush");
        assert_eq!(post_hits(&rec, "albatross").len(), 1);
    }

    /// **The trickle is never lease-gated** — the ruled carve-out, structural
    /// in the door (`content-index.md` § Where the index is built: *every live
    /// builder stages and publishes its own arrivals regardless of who holds
    /// the lease*). Posts never leave their catch-up window (the walk is
    /// permanently pass-shaped), so a trickle routed through `stage_doc`'s
    /// lease check would withhold a stood-down seat's own new post for the
    /// life of the session.
    #[tokio::test]
    async fn the_post_trickle_ignores_the_lease_stand_down() {
        let rec = Arc::new(Recorder::default());
        let gate = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let builder = posts_builder(Arc::clone(&rec)).with_lease_gate(gate);

        builder.stage_post_trickle(&post("cc", 3_000, "my own words, just composed"));
        builder.flush().await.expect("flush");
        assert_eq!(
            post_hits(&rec, "composed").len(),
            1,
            "a stood-down seat still stages its own arrival"
        );
    }

    /// An empty-bodied post (a video post — `Post::body_text` yields `""`) is
    /// skipped without burning the re-index guard: it has nothing searchable,
    /// and the guard's set means *indexed*, which this id is not.
    #[tokio::test]
    async fn an_empty_bodied_post_is_skipped_without_burning_the_guard() {
        let rec = Arc::new(Recorder::default());
        let builder = posts_builder(Arc::clone(&rec));

        builder.stage_post_trickle(&post("dd", 4_000, ""));
        assert!(builder.stage_posts_walk(&[post("ee", 5_000, "")]));

        assert_eq!(builder.pending_len(), 0, "nothing searchable was staged");
        assert_eq!(builder.indexed_len(), 0, "and no guard slot was burned");
    }

    /// Both posts doors respect the builder's kind set — the same wrong-class
    /// protection every other producer has.
    #[tokio::test]
    async fn a_non_posts_builder_ignores_both_doors() {
        let rec = Arc::new(Recorder::default());
        let builder = drafts_builder(Arc::clone(&rec));

        assert!(
            !builder.stage_posts_walk(&[post("aa", 1_000, "the albatross")]),
            "a walk against a builder that does not stage posts is not admitted"
        );
        builder.stage_post_trickle(&post("bb", 2_000, "the albatross"));
        assert_eq!(builder.pending_len(), 0);
    }

    // ── the files door ─────────────────────────────────────────────────────

    fn files_builder(rail: Arc<Recorder>) -> IndexBuilder {
        IndexBuilder::master(
            IndexMasterKey::from_bytes(MASTER),
            [ContentKind::File],
            rail,
        )
    }

    fn file(folder_id: i64, path_hash_hex: &str, path: &str) -> IndexableFile {
        IndexableFile {
            folder_id,
            path_hash_hex: path_hash_hex.to_string(),
            path: path.to_string(),
            updated_at_secs: 1_700_000_000,
        }
    }

    fn file_hits(rec: &Recorder, term: &str) -> Vec<ContentId> {
        master_kind_hits(rec, ContentKind::File, term)
    }

    /// The append shape, end to end — and the reason File needs **no corpus
    /// marker**: the stage-time guard alone makes an unchanged corpus stage
    /// nothing, so a full re-drain every sweep publishes zero rail bytes. That is
    /// the ruled suppression, so this test *is* the suppression's proof.
    #[tokio::test]
    async fn a_files_walk_stages_unseen_rows_and_a_redrain_republishes_nothing() {
        let rec = Arc::new(Recorder::default());
        let builder = files_builder(Arc::clone(&rec));

        assert!(builder.stage_files_walk(&[
            file(42, "aa", "holidays/albatross.jpg"),
            file(42, "bb", "receipts/provisions.pdf"),
        ]));
        builder.flush().await.expect("first flush");
        assert_eq!(file_hits(&rec, "albatross").len(), 1);
        assert_eq!(file_hits(&rec, "provisions").len(), 1);

        // The next sweep re-drains the same listing — the steady state.
        assert!(builder.stage_files_walk(&[
            file(42, "aa", "holidays/albatross.jpg"),
            file(42, "bb", "receipts/provisions.pdf"),
        ]));
        flush_none(&builder).await;
    }

    /// The identity is the **pair**, so the same relative path in two different
    /// sets is two documents. Keying on `path_hash` alone would have the second
    /// set's file silently guarded away as already-indexed.
    #[tokio::test]
    async fn the_same_path_in_two_sets_is_two_documents() {
        let rec = Arc::new(Recorder::default());
        let builder = files_builder(Arc::clone(&rec));

        assert!(builder.stage_files_walk(&[
            file(1, "aa", "notes/albatross.txt"),
            file(2, "aa", "notes/albatross.txt"),
        ]));
        builder.flush().await.expect("flush");

        let hits = file_hits(&rec, "albatross");
        assert_eq!(hits.len(), 2, "one doc per (set, path_hash) pair");
        let mut ids: Vec<String> = hits
            .iter()
            .map(|c| String::from_utf8(c.0.clone()).unwrap())
            .collect();
        ids.sort();
        assert_eq!(ids, vec!["1:aa".to_string(), "2:aa".to_string()]);
    }

    /// A files walk is **pass-shaped**, so the advisory lease gates it exactly as
    /// it gates every launch backlog. There is no trickle twin to carve out: file
    /// writers are out-of-process, so this arm has walk + push and nothing else.
    #[tokio::test]
    async fn a_stood_down_seats_files_walk_is_withheld_and_reports_it() {
        let rec = Arc::new(Recorder::default());
        let gate = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let builder = files_builder(Arc::clone(&rec)).with_lease_gate(Arc::clone(&gate));

        assert!(
            !builder.stage_files_walk(&[file(42, "aa", "holidays/albatross.jpg")]),
            "a closed gate withholds the pass-shaped walk"
        );
        flush_none(&builder).await;

        gate.store(true, std::sync::atomic::Ordering::Release);
        assert!(
            builder.stage_files_walk(&[file(42, "aa", "holidays/albatross.jpg")]),
            "gaining the lease admits the same pass"
        );
        builder.flush().await.expect("flush");
        assert_eq!(file_hits(&rec, "albatross").len(), 1);
    }

    /// **The arm's load-bearing degrade.** A row whose sealed label this seat
    /// cannot open arrives path-less; it must be skipped *without* burning the
    /// re-index guard, so a later walk on a seat that holds the set's keys stages
    /// it for real. Burning the guard here would make an unreadable row
    /// permanently unindexable on this device.
    #[tokio::test]
    async fn an_unrenderable_row_is_skipped_without_burning_the_guard() {
        let rec = Arc::new(Recorder::default());
        let builder = files_builder(Arc::clone(&rec));

        assert!(builder.stage_files_walk(&[file(42, "aa", "")]));
        assert_eq!(builder.pending_len(), 0, "nothing searchable was staged");
        assert_eq!(builder.indexed_len(), 0, "and no guard slot was burned");

        // The keys turned up; the same identity now stages for real.
        assert!(builder.stage_files_walk(&[file(42, "aa", "holidays/albatross.jpg")]));
        builder.flush().await.expect("flush");
        assert_eq!(file_hits(&rec, "albatross").len(), 1);
    }

    /// Every way a user might reach a file by name.
    ///
    /// **The bare-stem case is the one with teeth.** The shared tokenizer welds
    /// an extension to its stem (`albatross.jpg` is one token — pinned in
    /// `fauna_mail::tokenizer`), so indexing the path alone would leave a user
    /// typing `albatross` with no hit at all. `file_doc_for` derives the stem and
    /// the extension for exactly that reason; this is the pin.
    ///
    /// Mutation this reddens under: dropping either derived field from
    /// `file_doc_for` and indexing only the basename + path.
    #[tokio::test]
    async fn a_file_is_findable_by_stem_folder_full_name_and_extension() {
        let rec = Arc::new(Recorder::default());
        let builder = files_builder(Arc::clone(&rec));

        assert!(builder.stage_files_walk(&[file(42, "aa", "holidays/albatross.jpg")]));
        builder.flush().await.expect("flush");

        assert_eq!(
            file_hits(&rec, "albatross").len(),
            1,
            "the bare stem — what a user actually types, and what the path alone \
             would NOT match"
        );
        assert_eq!(file_hits(&rec, "holidays").len(), 1, "by folder segment");
        assert_eq!(
            file_hits(&rec, "albatross.jpg").len(),
            1,
            "by the whole file name"
        );
        assert_eq!(file_hits(&rec, "jpg").len(), 1, "by file type");
    }

    /// A dotfile is a *name*, not a stem-plus-extension: `.bashrc` must stay one
    /// term rather than minting an empty stem and an extension `bashrc` that
    /// would rank every dotfile under the same token.
    #[tokio::test]
    async fn a_dotfile_keeps_its_whole_name_and_yields_no_extension() {
        let rec = Arc::new(Recorder::default());
        let builder = files_builder(Arc::clone(&rec));

        assert!(builder.stage_files_walk(&[file(42, "aa", "home/.bashrc")]));
        builder.flush().await.expect("flush");

        assert_eq!(file_hits(&rec, "bashrc").len(), 1, "found by its name");
        assert_eq!(file_hits(&rec, "home").len(), 1, "and by its folder");
    }

    /// The files door respects the builder's kind set — the same wrong-class
    /// protection every other producer has.
    #[tokio::test]
    async fn a_non_files_builder_ignores_the_door() {
        let rec = Arc::new(Recorder::default());
        let builder = drafts_builder(Arc::clone(&rec));

        assert!(
            !builder.stage_files_walk(&[file(42, "aa", "holidays/albatross.jpg")]),
            "a walk against a builder that does not stage files is not admitted"
        );
        assert_eq!(builder.pending_len(), 0);
    }
}
