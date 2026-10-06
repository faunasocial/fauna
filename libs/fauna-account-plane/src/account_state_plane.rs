//! The class-2 client leg of the generalized account-data feed: publish a
//! sealed state entry, walk the scope's feed, merge what comes back (W2.4 (account-data-plane.md § Workstreams)).
//!
//! Authority: `docs/goal/architecture/account-sync-plane.md` § Feeds and
//! cursors (the frontier vector + its accounting law), § The class-2 entry form
//! (T14 — the sealed envelope this module mints and opens), § Merge-policy seam
//! (dispatched through [`fauna_protocol::merge_policy`]), § Nudges and
//! backstops (the two backstops [`AccountStatePlane::walk`] and
//! [`AccountStatePlane::reconcile`] are). The slice's own build contract is
//! tracked internally and not shipped; everything binding on a reader of this
//! module is in the goal doc above or stated here.
//!
//! # The three planes this sits between
//!
//! - **Local**: [`AccountStore`] — the reading replica's plaintext store
//!   (charter § Local at-rest posture, R3 (account-data-plane.md § The ratified decisions): "the app-side replica rests
//!   plaintext in the user's OS context"). Values and merge metadata are stored
//!   opened; the store never holds a key.
//! - **Wire**: `fauna.account.state.put` / `fauna.sync.changes.list`, carrying
//!   the sealed T14 envelope and nothing but the cleartext floor.
//! - **Keys**: `AccountStateKeySchedule`, derived from the owner `BackupKey`
//!   this replica's bundle already carries.
//!
//! So sealing happens *at publish* and opening *at ingest*: the plaintext half
//! never reaches the nest and the sealed half never reaches the store.
//!
//! # Why the seal happens after the local write, not before
//!
//! The T14 AAD binds `{form_version, writer_id, writer_seq, scope, item_key}`,
//! and `writer_seq` is assigned by the store when the entry lands on the local
//! journal — so the envelope cannot exist until the local write has happened.
//! That ordering is also the durability one we want: the user's write is on
//! disk before any network call, and an unpublished local row is recoverable
//! ([`AccountStatePlane::publish_pending`]).
//!
//! # How a reader learns which kind a row belongs to
//!
//! It does not — it *tries*. A feed row names its item only by the blinded item
//! key `keyed_hash(item_blind(kind), logical_key)`, which is one-way by design
//! (§ The class-2 entry form: an unkeyed hash "would let any custodian
//! dictionary *which setting* changed"). So the reader opens under each
//! registered kind's entry key in turn and lets the AEAD tag arbitrate — the
//! same trial-chain shape the shipped epoch-aware mail opener uses
//! (`owner-key-material.md` § Path B-sibling-2). A row that opens under none of
//! them is left alone rather than guessed at; see [`WalkReport::unopened`].
//!
//! # A predecessor's rows, carried and listed
//!
//! On a successor's bound delegable plane the trial chain has one more link:
//! an attested predecessor's delegable schedule
//! ([`AccountStatePlane::with_predecessor_schedules`]). A row that opens there
//! is carried — merged, and re-authored as this replica's own row when that
//! changes the entry ([`WalkReport::inherited`]) — and a full-state reconcile
//! also lists it, by its served coordinates and the item it opened to
//! ([`AccountStatePlane::inherited_rows`]): the candidates the pass's retire
//! behind the carry judges (`crate::delegable_reclaim`;
//! `succession-aftermath.md` § Re-key scope → *The predecessor's own row is
//! retired behind the carry*). A carry the nest refuses `scope_full` never
//! aborts the walk, so the listing that licenses those retires is still
//! banked: on this scope the publish parks the re-authored row
//! ([`AccountStatePlane::publish_pending`]), and a refusal that does reach
//! the walk is counted ([`WalkReport::carry_scope_full`]) and passed over.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use ed25519_dalek::SigningKey;
use fauna_account_store::{
    backend::StoreBackend,
    store::{AccountStore, WriterRelation},
    types::{ItemRef, JournalOp, JournalRow, RelayRow, StateEntry, WriterId},
};
use fauna_core::account_entry_crypto::{
    EntryCoordinates, EntryPlaintext, open_entry, peek_generation_id, seal_entry, seal_entry_v2,
    sealed_envelope_len,
};
use fauna_core::crypto::{
    AccountStateKeySchedule, AccountStateKindKeys, DelegableSchedule, FleetOnlySchedule,
    SealingEpoch,
};
use fauna_core::generation::{AdmissibleTip, TipResolution};
use fauna_protocol::RpcRequester;
use fauna_protocol::account_state::{
    ACCOUNT_STATE_FLEET_SCOPE, ACCOUNT_STATE_SCOPE, AccountStatePutReply, AccountStatePutRequest,
    ItemClass, KIND_STATE_PUT, MAX_REPLACED_ROWS_PER_PUT, MAX_STATE_ENTRY_BYTES, OP_STATE_PUT,
    OP_TOMBSTONE, ReplacedRow,
};
use fauna_protocol::merge_policy::{
    AdmittedKinds, LwwStamp, MergeOutcome, apply_class2, class2_kinds, delegable_kinds,
    home_scope_for_kind, sealing_epoch,
};
use fauna_protocol::sync::{SyncChange, SyncChangesListReply, SyncChangesListRequest};

use crate::generation_tip::{self, GenerationTrust};

/// A relay row's coordinate: `(writer, writer seq, blinded item key)`.
type RelayCoordinate = ([u8; 32], u64, [u8; 32]);

/// The kind + logical key of one class-2 item.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ItemId {
    pub kind: String,
    pub key: String,
}

/// What the nest answered a [`AccountStatePlane::retire`] — a verdict, never
/// an error (charter § The generation machinery → *Fleet-scope reclamation*,
/// clause (1)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetireOutcome {
    /// The live row at those coordinates is now superseded on the feed.
    Retired,
    /// No live row sits at those coordinates any more — already retired,
    /// superseded by a newer row of the same writer, or never published.
    Gone,
    /// The retention gate: a marked walker holding a live grant has not walked
    /// past the row. Ask again next pass.
    NotYetStable,
    /// The generation belt: a live row is still sealed under the generation
    /// the caller asserted dataless. Ask again next pass.
    GenerationInUse,
    /// The leg cannot carry the kind (the peer leg). The row stays — today's growth.
    Unsupported,
}

/// Refuse `plaintext` if its sealed envelope — in its kind's registered form,
/// v2 for a `GenerationTip` kind and v1 otherwise — would exceed
/// [`MAX_STATE_ENTRY_BYTES`], the per-entry ceiling every nest enforces on
/// `fauna.account.state.put`.
///
/// Sized from the encodings alone ([`sealed_envelope_len`]), so it runs before
/// the entry has a writer seq, which is the point: an entry over the cap can
/// never publish, and a durable local row of one would be re-sent and refused
/// on every [`AccountStatePlane::publish_pending`] pass — which stops at its
/// first failure, holding every later row this writer puts in the scope
/// local-only behind it. Never minting such a row is the whole contract: the
/// pass has no heal for one, because no row this plane journals can be one.
///
/// **Nothing calls this directly.** [`SizedEntry::size`] is the only way to
/// ask it and [`AccountStatePlane::put_own_row`] — the plane's one write onto
/// this replica's own log — is the only way to spend the answer, so every arm
/// that journals a value this replica authored passes the door by
/// construction: there is no row to hand the store without a ticket, and no
/// ticket without this check. It was four hand-wired calls and a fifth write
/// exempted "bounded by construction" before that, and the copy the walk's
/// `Replace` + [`Take::Carry`] arm carried was reachable by no test at all
/// — deleting it red nothing.
fn refuse_if_over_entry_cap(plaintext: &EntryPlaintext) -> Result<()> {
    let generation_sealed = matches!(
        sealing_epoch(&plaintext.kind),
        Some(SealingEpoch::GenerationTip)
    );
    let len = sealed_envelope_len(plaintext, generation_sealed)
        .context("account-state put: sizing the sealed entry")?;
    if len > MAX_STATE_ENTRY_BYTES {
        bail!(
            "a {:?} entry seals to {len} bytes, over the {MAX_STATE_ENTRY_BYTES}-byte per-entry \
             cap every nest enforces — refused before the local write: no nest accepts it, and a \
             local row of it would be refused on every publish pass while holding this writer's \
             later rows in the scope behind it",
            plaintext.kind
        );
    }
    Ok(())
}

/// One [`EntryPlaintext`] that has passed [`refuse_if_over_entry_cap`] — the
/// door's verdict carried as a value.
///
/// [`AccountStatePlane::put_own_row`] takes one of these and nothing else, so
/// the cap is not a step an arm remembers: it is the ticket the write demands.
///
/// **Why a ticket and not a check at the head of the write.** The rule this
/// implements (`security/review-method.md` ⭐ *A guard whose failure is silent
/// lives inside the consumer*) puts the guard at the head of the consuming
/// function. That is the right home when the caller can pass the door at the
/// moment it writes — but [`AccountStatePlane::write_local_and_publish`]
/// cannot: [`AccountStatePlane::admit_origination`] sits between its door and
/// its write and can **mint a generation** (the mint protocol's trigger (a)),
/// and an entry that can never publish must not spend one, nor lose its own
/// refusal to the tip's. Carrying the verdict instead of checking at the write
/// decouples *when the door is passed* from *when the row lands*, and still
/// leaves the store unreachable without it.
///
/// The borrow is deliberate: the ticket cannot outlive the plaintext it
/// vouched for, so it can never be spent on a different value.
pub struct SizedEntry<'a>(&'a EntryPlaintext);

impl<'a> SizedEntry<'a> {
    /// Pass the per-entry cap door, and on success hold its verdict.
    pub fn size(plaintext: &'a EntryPlaintext) -> Result<Self> {
        refuse_if_over_entry_cap(plaintext)?;
        Ok(Self(plaintext))
    }

    /// The value the door admitted.
    fn plaintext(&self) -> &'a EntryPlaintext {
        self.0
    }
}

/// How the walk lands a served row once it is past the own/retired arms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Take {
    /// Another writer's row, or retired history at a coordinate this journal
    /// does not hold: journaled at its own coordinate, as ever.
    Ingest,
    /// A row whose coordinate this replica must not journal — one this
    /// journal holds under another item, or a predecessor identity's
    /// (`account-replica-posture.md` § The store device principal,
    /// refinement 11, the three carry arms): the value is merged and, when it
    /// changes the entry, re-journaled as this replica's own row — nothing
    /// is ever written at the served coordinate. [`Carried`] names which arm.
    Carry(Carried),
}

/// Which of refinement 11's three carry arms a [`Take::Carry`] is. What they
/// do to the entry and to this replica's own log is identical; they differ in
/// the relay plane (a refused row's relay row is retired; a served row's is
/// kept) and in what the re-authored row is sealed under (always this
/// replica's own schedule — for an inherited row that IS the point), and each
/// has its own counter so the shapes stay distinguishable in a report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Carried {
    /// A retired burnt writer's row at a coordinate this journal still
    /// holds. See [`WalkReport::retired_carried`].
    RetiredBurnt,
    /// A foreign writer's second row at a coordinate this journal already
    /// holds under a DIFFERENT item — the double-served coordinate. See
    /// [`WalkReport::double_served`].
    DoubleServed,
    /// An attested predecessor identity's generation-0 row, which opened
    /// under that identity's retired keys and not under this replica's own:
    /// a delegable row, or — on the fleet scope — the mint record of a
    /// generation whose key this device holds. See [`WalkReport::inherited`].
    Inherited,
}

impl WalkReport {
    /// A carried row's publish ([`AccountStatePlane::publish_own_rows_through`])
    /// came back `result`: a `scope_full` refusal is counted and the walk
    /// goes on; every other failure is the walk's.
    fn tolerate_carry_scope_full(&mut self, result: Result<()>) -> Result<()> {
        match result {
            Err(err) if is_scope_full(&err) => {
                self.carry_scope_full += 1;
                Ok(())
            }
            other => other,
        }
    }

    fn count_carry(&mut self, why: Carried) {
        match why {
            Carried::RetiredBurnt => self.retired_carried += 1,
            Carried::DoubleServed => self.double_served += 1,
            Carried::Inherited => self.inherited += 1,
        }
    }
}

/// One served state-entry row, past the walk's coordinate checks.
struct Served<'a> {
    change: &'a SyncChange,
    writer: WriterId,
    origin_seq: u64,
    item_key: [u8; 32],
    envelope: &'a [u8],
}

/// What one walk did. Every row a walk saw lands in exactly one counter, so a
/// caller can tell "nothing to do" from "I skipped everything".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WalkReport {
    /// Feed pages fetched.
    pub pages: usize,
    /// Rows seen across those pages.
    pub rows: usize,
    /// Rows whose value this replica adopted verbatim.
    pub applied: usize,
    /// Rows this replica merged into a new local value (and re-published).
    pub merged: usize,
    /// Rows whose value lost to the local one — the row was still journaled and
    /// accounted, only the entry stayed put.
    pub kept: usize,
    /// Rows this replica authored, coming back off the feed.
    pub self_echo: usize,
    /// Rows under this replica's own CURRENT writer that this journal does
    /// not hold — above everything it holds under that writer, or at a held
    /// coordinate under a different item: the **burnt-journal signature**
    /// (`account-replica-posture.md` § The store device principal,
    /// refinement 11). A writer lives exactly as long as its journal, so
    /// these rows were authored by a journal this one is not (a store dir
    /// restored from an older backup), and every seq this journal issues
    /// next may collide with them. Refused — never ingested, never echoed,
    /// the frontier left below them so they stay served — and the writer is
    /// marked burnt for the assembly's heal arm to rotate away; the pump
    /// reassembles on a non-zero count. Zero on every healthy walk.
    pub own_burnt: usize,
    /// Rows under this store's RETIRED writer whose journal a walk found
    /// burnt (`burnt_writer_id` names it — the verdict outlives the fence),
    /// served at a coordinate that journal still holds: **carried** rather
    /// than echoed (`account-replica-posture.md` § The store device
    /// principal, refinement 11 → *the retired burnt writer's rows are
    /// carried*). The held row is the burnt life's — which the fleet may
    /// hold under another item, under the same item with another value, or
    /// not at all — so the coordinate vouches for nothing: the served value
    /// goes through the ordinary class-2 apply and, when it changes the
    /// entry, is re-journaled as this replica's own row; nothing is written
    /// at the retired coordinate, and the burnt life's relay row there is
    /// retired for the fleet's. Idempotent, so a coordinate the feed serves
    /// twice (a peer still relaying the burnt life's row beside the nest's)
    /// is stable across passes. Zero for a store no walk ever found burnt.
    pub retired_carried: usize,
    /// A FOREIGN writer's second row at a coordinate this journal already
    /// holds under a DIFFERENT item — the double-served coordinate
    /// (`account-replica-posture.md` § The store device principal,
    /// refinement 11 → *a foreign writer's second row at a held coordinate
    /// is carried*). The nest refuses a reused `(scope, writer, seq)`, but the
    /// relay plane records an own row BEFORE its send, so a peer can pull a
    /// burnt writer's row at a coordinate the nest then refuses (another
    /// life spent it) and relay it for good beside the nest's row; the first
    /// ingests as ever, and the second — which used to abort the walk's page
    /// as journal equivocation, every full-pass reconcile, forever — is
    /// carried exactly as a retired burnt writer's row is:
    /// merged through the ordinary class-2 apply, re-journaled as this
    /// replica's own row only when it changes the entry, nothing written at
    /// the coordinate, and BOTH relay rows kept (a peer is served what the
    /// nest serves). Distinct from [`Self::retired_carried`] so a genuinely
    /// equivocating writer stays visible: non-zero means the feed holds a
    /// duplicate coordinate — a relayed burnt row, or a writer that reused a
    /// seq. A same-item row with other content at a held coordinate
    /// is NOT this: the nest collapses per `(item, writer)`, so that shape is
    /// corruption, and `ingest_state`'s refusal still aborts on it.
    pub double_served: usize,
    /// An attested predecessor identity's generation-0 **delegable** rows,
    /// **carried** across a succession (`succession-aftermath.md` § Re-key
    /// scope → *The account-state plane's generation-0 delegable rows are
    /// carried by the successor's walk*). The row is sealed under keys derived
    /// from the predecessor's `BackupKey`, so it does not open under this
    /// replica's schedule; the plane was handed that identity's delegable
    /// schedule ([`AccountStatePlane::with_predecessor_schedules`]) and
    /// opened it there. It goes through the ordinary class-2 apply and, when
    /// that changes the entry, is re-journaled as this replica's own row —
    /// value and `merge_meta` verbatim — so it publishes sealed under THIS
    /// identity's schedule and a device holding only the successor seed reads
    /// it. Nothing is written at the predecessor writer's coordinate, and a
    /// row the entry already covers writes nothing, so every later
    /// presentation (each full-state reconcile re-serves it) is counted here
    /// and is otherwise a no-op.
    ///
    /// The fleet scope's plane counts ONE kind here, by the same carry: a
    /// predecessor's `fauna.state.generation-mint` row that opened under
    /// that identity's mint-kind keys
    /// ([`AccountStatePlane::with_predecessor_mint_keys`]) and is the true
    /// record of a generation this device already keys
    /// (`owner-key-material.md` § Path A-sibling-2 → *Rotation*, the
    /// succession rider → *What crosses*). Every other fleet-only row of a
    /// predecessor stays [`Self::unopened`], as does a mint row that fails
    /// that test.
    ///
    /// Zero on a plane handed no predecessor material — every account that
    /// never succeeded, and every leg but the bound nest's.
    pub inherited: usize,
    /// Carried rows (any [`Take::Carry`] arm) whose re-authored own row's
    /// publish came back `scope_full` (`succession-aftermath.md` § Re-key
    /// scope → *The predecessor's own row is retired behind the carry* →
    /// *At the cap*). Counted, and the walk goes on: the own row stays
    /// journaled and [`AccountStatePlane::publish_pending`] sends it once a
    /// retire has freed a pair. Only the fleet plane's publish still answers
    /// so — the delegable plane parks the row instead (refinement 11 → *A row
    /// refused for room is parked*), so there this stays 0. A carry refused
    /// for any other reason still fails the walk. Also counted in the carry's
    /// own arm counter.
    pub carry_scope_full: usize,
    /// Rows that opened under no registered kind, and were therefore skipped
    /// without being accounted.
    ///
    /// The expected causes are both benign and both *forward*-looking: a writer
    /// running a newer build sealed a kind this binary predates, or this
    /// principal holds a per-kind grant rather than the whole schedule. Skipping
    /// is the compat-correct answer — the plane is state-based, so
    /// [`AccountStatePlane::reconcile`] re-presents the row once the reader can
    /// open it (charter § Feeds and cursors, backstop 2). A tampered or
    /// truncated envelope lands here too, which is why the count is reported
    /// rather than swallowed.
    pub unopened: usize,
    /// The generations under which this walk left a row [`Self::unopened`]
    /// because this device held no key for that generation — the
    /// generation-sealed form names its generation in cleartext, and
    /// `generation_key_for` answered none. Only that arm: a row that fails to
    /// open under a key the device holds names nothing here. What the bound
    /// plane records as the store's unkeyed set
    /// (`AccountStore::unkeyed`; `account-client-lifecycle.md` § The
    /// client-side lifecycle → *The first listing*, clause (5)).
    pub unkeyed: std::collections::BTreeSet<[u8; 32]>,
    /// Rows that opened, and named a kind this build knows, but which no build
    /// will ever apply *here* — a tombstone on a `CrdtPerField` kind, a
    /// stampless or stamp-malformed `LatestWins` row, a CRDT value that will
    /// not decode (`MergeError::is_row_content` is the classifier), or a row
    /// riding a scope its kind never seals into (the A5 partition's read-side
    /// mirror of the writer door's home-scope guard). Skipped without being
    /// accounted, exactly like [`Self::unopened`], and counted on every walk.
    ///
    /// Unlike `unopened` the cause is never benign: the writer door refuses
    /// these ([`AccountStatePlane::write_local_and_publish`]), so one on the
    /// feed means a hostile device (any fleet-key holder — a *removed* device
    /// retains `BackupKey` forever), or a
    /// tampered row. It is nonetheless **skipped rather than fatal**, and the
    /// invariant decides that rather than taste: the nest collapses per
    /// `(item_key, writer)`, so a row its author never supersedes is permanent,
    /// and aborting the walk on it would starve this replica of every *other*
    /// writer's rows forever — a client-causable state no client could
    /// recover from (`nest/common.md` § Client-state recoverability). Skipping
    /// costs only that one item; the count is what keeps it from being silent.
    ///
    /// A record of a closed-by-design type that this build cannot decode —
    /// met at an occupied cell as well as at first contact, since every join
    /// over such a type fails rather than rank it (`transport.md` § Schema and
    /// forward-compat discipline → *Rule 3 in full*, the `consensus` ground)
    /// — counts here too, and is no less abnormal: a new variant of such a
    /// type is a major-version change, so inside a major it can only be junk.
    /// The skip is still the right reading of it, because it leaves the row
    /// unaccounted and so presented again to any build that can read it.
    pub unmergeable: usize,
}

impl crate::page_walk::PageTally for WalkReport {
    fn page_fetched(&mut self) {
        self.pages += 1;
    }
}

/// The nest-log cursor, unused by this feed.
///
/// `since` is the **nest-writer's** slot (charter § The frontier vector:
/// "today's scalar `since` cursor *is* the frontier `{nest: since}`"), and every
/// class-2 row is authored by a *device* writer — `fauna.account.state.put`
/// carries a `writer_id` — so the nest gates all of them on the frontier map
/// and none on `since` (`bins/fauna-nest/src/db/account_state.rs`
/// `get_account_state_changes`). Sending 0 is therefore not a full re-read: it
/// is the complete and correct nest-writer frontier for a feed with no
/// nest-authored rows.
pub const NEST_SLOT_UNUSED: i64 = 0;

/// A first-need heal-mint's parent list: the resolver's leaf set, capped at
/// [`fauna_core::generation::MAX_MINT_PARENTS`] preferring the byte-order max
/// (deterministic under an attacker's row flood; charter § The mint protocol).
/// Leaves arrive ascending from the resolver, so the cap keeps the tail.
///
/// Shared with the group plane's severance mint
/// (`fauna_sync_engine::group_authority_revocation`): the two DAGs are different, the
/// parent-selection rule is one.
pub fn heal_parents(leaf_ids: &[[u8; 32]]) -> Vec<[u8; 32]> {
    let cap = fauna_core::generation::MAX_MINT_PARENTS;
    let start = leaf_ids.len().saturating_sub(cap);
    leaf_ids[start..].to_vec()
}

/// The class-2 leg of one account scope: publish, walk, reconcile.
pub struct AccountStatePlane<'a, B: StoreBackend, R: RpcRequester> {
    store: &'a AccountStore<B>,
    rpc: &'a R,
    schedule: &'a AccountStateKeySchedule,
    /// The authoring device's signing key — the R13 in-seal writer signature
    /// (`account-data-plane.md` § The class-2 entry form). Its public half is the
    /// store's `writer()`, which [`Self::new`] refuses to let diverge.
    writer_key: &'a SigningKey,
    /// The identity line + escrow-holder trust the writer door's tip
    /// resolution consumes (`crate::generation_tip`). Only *sealing* consults
    /// it — the walk's v2 open path is integrity-only by design
    /// (`generation_tip` module docs: admissibility gates sealing, never
    /// reading).
    trust: &'a GenerationTrust,
    scope: String,
    /// `false` for the **peer leg** ([`Self::new_pull_only`]): a merge's
    /// output is written locally (and into the relay plane, sealed) but never
    /// RPC-published — the peer channel serves no write kind, and peer
    /// convergence is pull-both-ways (the other side pulls our merged row;
    /// the nest gets it from the nest leg's [`Self::publish_pending`], which
    /// finds it because our own frontier slot — the published high-water —
    /// deliberately does not advance here).
    publish_to_feed: bool,
    /// `true` for a **linked nest's** plane ([`Self::new_linked`] —
    /// `account-sync-plane.md` § The bind leg, ruling 4): a nest this runtime
    /// completes as a secondary replica beside the one it is bound to. It
    /// walks (a full-state reconcile, its answer this nest's listing), is
    /// pushed the diff and asked to retire — but every piece of state that
    /// names *the bound nest* is left alone: no watermark is sent, banked or
    /// voided, no relay row's serve coordinate is stamped from its log, our
    /// own frontier slot (the bound nest's published high-water) never
    /// advances on its echo, no walker mark is left there, and the journal
    /// publish and the mint are the bound plane's alone.
    linked: bool,
    /// `true` for a **throwaway replica's** plane ([`Self::new_fold_only`]):
    /// a value this plane authors during a walk — a merge's output, a carry —
    /// is folded into its own store and never sealed. The replica trusts no
    /// escrow holder, so no tip resolves for it and a `GenerationTip` seal is
    /// refused; and its writer key is ephemeral, so a row it sealed would
    /// serve no one. Its reads are what it is for.
    fold_only: bool,
    /// Single-flight for the **first-need mint** (trigger (a)). Concurrent
    /// `GenerationTip` originations on one plane would otherwise each find no
    /// tip and each mint, forking the DAG on the first write of a fresh
    /// account; under this lock the second re-resolves and finds the first
    /// one's tip.
    ///
    /// **This is the W5 seam, deliberately process-scoped.** The charter's
    /// "any enrolled device may mint — convergence handles forks — but the
    /// engine-singleton role (W5) is the *preferred* minter per machine" wants
    /// the trigger W5-*aware* without being W5-*blocked*: cross-process
    /// preference is W5's to add here later, and until it exists a
    /// cross-process fork is the ratified, convergent outcome rather than a
    /// bug this lock must pretend to solve.
    mint_lock: tokio::sync::Mutex<()>,
    /// The retained-bundle custody the key-obtaining seams consult and feed
    /// (W5.4a — `generation_tip::RetainedKeyCustody` owns the contract).
    /// `None` (tests, surfaces with no slot) is plane-native behavior,
    /// unchanged.
    custody: Option<&'a dyn crate::generation_tip::RetainedKeyCustody>,
    /// The attested predecessor identities' generation-0 **delegable**
    /// schedules — trial-open keys for [`Take::Carry`]'s inherited arm
    /// ([`Self::with_predecessor_schedules`]). Read-only by construction:
    /// nothing in this type seals under one. Empty (every plane but a
    /// successor's delegable-scope one) is plane-native behavior, unchanged.
    predecessors: &'a [DelegableSchedule],
    /// The attested predecessor identities' generation-0 keys for the ONE
    /// fleet-only kind `fauna.state.generation-mint` — trial-open keys for
    /// the mint-record carry ([`Self::with_predecessor_mint_keys`]).
    /// Read-only by construction, like the schedules above. Empty (every
    /// plane but a successor's bound fleet-scope one) is plane-native
    /// behavior, unchanged.
    predecessor_mint_keys: &'a [AccountStateKindKeys],
    /// The attested predecessor identities' generation-0 keys for EVERY
    /// fleet-only machinery kind — the reclamation pass's keys for clause
    /// (3)(i)'s predecessor arm ([`Self::with_predecessor_machinery_keys`]),
    /// tried by [`Self::open_predecessor_machinery_row`] alone: no walk, no
    /// [`Self::open_relay_row`], nothing that merges or vouches. Empty (every
    /// plane but a successor's bound fleet-scope one) is plane-native
    /// behavior, unchanged.
    predecessor_machinery_keys: &'a [AccountStateKindKeys],
    /// The index into [`Self::predecessor_machinery_keys`] of the pair that
    /// opened the last row — the predecessor arm's ordering hint, the
    /// [`Self::trial_hint`] shape: a predecessor's machinery is served in
    /// runs of one kind.
    machinery_hint: std::sync::Mutex<Option<usize>>,
    /// The relay rows, by `(writer, writer seq, item key)`, that opened under
    /// no retired machinery pair — not tried again while this plane lives,
    /// so a row the arm cannot open costs its AEAD attempts once, not every
    /// pass. A coordinate names one sealed row (the nest collapses per
    /// `(item, writer)`, and a writer seq is never reused), so a miss stays
    /// a miss.
    machinery_misses: std::sync::Mutex<std::collections::BTreeSet<RelayCoordinate>>,
    /// Is this publish error the nest's **final refusal** of the coordinate
    /// (`stale_writer_seq`)? Resolved at construction — [`Self::new`] binds
    /// the typed classifier under an `RpcErrorClass` bound on the
    /// requester's error, [`Self::new_pull_only`] binds "never" (the peer leg
    /// sends nothing) — so [`Self::publish`] stays generic over a bare
    /// `Display` error and the ~30 pull-only requesters keep their `anyhow`
    /// errors. What a final refusal does: `account-replica-posture.md` § The
    /// store device principal, refinement 11 → *a refused row's relay
    /// residue*.
    refused_for_good: fn(&R::Error) -> bool,
    /// The kind that opened the last walked row — [`Self::trial_kinds`]'s
    /// ordering hint. Pure performance: it changes which kind is TRIED first,
    /// never which kinds are tried.
    trial_hint: std::sync::Mutex<Option<&'static str>>,
    /// The feed's retention-gate watermark as the last walk page read it
    /// (`SyncChangesListReply::retirable_through_seq`): every live row at or
    /// below it passes the gate now; one above it is refused `not_yet_stable`.
    /// Per plane, never persisted — a pass's reclamation step reads the walk
    /// that ran just before it, and a stale value costs one deferred verdict,
    /// never a wrong retire (the nest gates every retire regardless). `None`
    /// until a reply carries it, and on the peer leg:
    /// then nothing is withheld, exactly today's behaviour.
    retirable_through: std::sync::Mutex<Option<u64>>,
    /// **This pass's listing** (`account-sync-plane.md` § The bind leg,
    /// ruling 1): every live `(writer, item)` row the nest served the last
    /// COMPLETED full-state reconcile, at its `writer_seq`, raised by every put
    /// the nest acked since. `None` from the moment a reconcile starts until
    /// it completes, on the peer leg, and before the first one: then nothing
    /// is known published ([`Self::listed_at_or_above`]), and the publish diff
    /// has nothing to diff against. Per plane, never persisted — it is a
    /// statement about the nest this runtime is bound to right now.
    listing: std::sync::Mutex<Option<Listing>>,
    /// The listing a reconcile in flight is collecting, row by row, as
    /// [`Self::apply`] sees them; moved into [`Self::listing`] when the walk
    /// completes, dropped when it fails.
    collecting: std::sync::Mutex<Option<Listing>>,
    /// **This pass's serve order** (`delegable-scope-reclamation.md`
    /// § Delegable-scope reclamation, the definition of *below*): for every
    /// row in [`Self::listing`], the position in THIS plane's nest's log the
    /// reconcile was served it at, raised by the position every put this
    /// plane's nest acked since names. Same lifecycle as the listing. It is
    /// the plane's own nest's order, a linked nest's included — unlike a
    /// relay row's `feed_seq`, which is the bound nest's alone.
    served: std::sync::Mutex<Option<ServeOrder>>,
    /// The serve order a reconcile in flight is collecting.
    collecting_served: std::sync::Mutex<Option<ServeOrder>>,
    /// The rows a carried own row covers that no relay row at its own item key
    /// shows — the predecessor's row it was carried from
    /// (`delegable-scope-reclamation.md` § Delegable-scope reclamation, part
    /// (2)) — by the own `writer_seq` the carry journaled. [`Self::publish`]
    /// names them beside the rows it finds itself, whichever path sends the
    /// row; in memory only, so a row a later process sends names what that
    /// process finds, and the retire behind the carry takes the rest.
    carry_names: std::sync::Mutex<BTreeMap<u64, Vec<RelayRow>>>,
    /// **The walk's inherited rows** (`succession-aftermath.md` § Re-key
    /// scope → *The predecessor's own row is retired behind the carry*,
    /// clause (1)): every attested predecessor's delegable row the last
    /// COMPLETED full-state reconcile opened under a retired schedule and
    /// merged — the retire candidates. Same lifecycle as [`Self::listing`]:
    /// `None` from the moment a reconcile starts until it completes. Per
    /// plane, never persisted.
    inherited_rows: std::sync::Mutex<Option<Vec<InheritedRow>>>,
    /// The inherited rows a reconcile in flight is collecting, as
    /// [`Self::take`] lands them; moved into [`Self::inherited_rows`] when
    /// the walk completes, dropped when it fails.
    collecting_inherited: std::sync::Mutex<Option<Vec<InheritedRow>>>,
    /// **The walk's unkeyed predecessor mints** (`owner-key-material.md`
    /// § Path A-sibling-2 → *Rotation*, the succession rider → *The kept
    /// wrap*): every predecessor mint record the last COMPLETED full-state
    /// reconcile opened under the mint-kind keys and did not carry for want
    /// of the key — condition (a) met, (b) not
    /// ([`Self::trial_open_predecessor_mint`]). The escrow-recovery pass asks
    /// the holder for each. Same lifecycle as [`Self::inherited_rows`]; per
    /// plane, never persisted.
    unkeyed_predecessor_mints: std::sync::Mutex<Option<Vec<UnkeyedPredecessorMint>>>,
    /// The unkeyed predecessor mints a reconcile in flight is collecting.
    collecting_unkeyed_predecessor_mints: std::sync::Mutex<Option<Vec<UnkeyedPredecessorMint>>>,
    /// The runtime's record of this process's first listings
    /// ([`Self::with_first_listings`]): told the moment this plane records
    /// its scope **listed**, so a read waiting at the gate returns. `None`
    /// outside the account driver; the durable fact is recorded either way.
    first_listings: Option<&'a FirstListings>,
    /// A listing of this scope ran to its end and left rows unopened, so the
    /// listed fact waits for the pass that ran it to end ([`Self::pass_ended`]
    /// — by then the pass has re-presented what its escrow recovery keyed).
    /// Cleared as each listing opens, so a pass that was cut cannot lend its
    /// listing to a later one.
    listing_owed: std::sync::atomic::AtomicBool,
    /// The account's **admitted-kinds overlay** (`third-party-kinds.md`
    /// § The kinds vocabulary → *The registry overlay*;
    /// [`Self::with_admitted_kinds`]): every registry lookup this plane makes
    /// — policy, epoch, keys — answers the compiled table first and then
    /// this. `None` is the empty overlay, plane-native behavior.
    admitted: Option<&'a AdmittedKinds>,
    /// An `ext:<kind>` plane's writer authority for the walk in flight
    /// ([`crate::ext_writers::ExtWriters`]) — read from the store as each
    /// walk opens, so a grant event merged between two walks admits its
    /// writer's rows at the next one. `None` on every other scope, which
    /// admits any writer the seal verifies (plane-native behavior).
    ext_writers: std::sync::Mutex<Option<crate::ext_writers::ExtWriters>>,
}

/// The empty overlay a plane with none consults.
static NO_ADMITTED_KINDS: AdmittedKinds = AdmittedKinds::new();

/// **This process's first listings** — which account-state scopes it has
/// recorded listed since it started, and which have served their launch wait
/// (`account-client-lifecycle.md` § The client-side lifecycle → *The first
/// listing*, clause (2)). The in-process half of the first-listing gate: the
/// durable fact is the store's ([`AccountStore::listed`]); this is what lets a
/// read waiting on it return the moment the bound plane records it, with no
/// poll. One per runtime, shared by its handle and its two bound planes, and
/// kept across a reassembly — the process's first listing stays its first.
#[derive(Debug, Default)]
pub struct FirstListings {
    listed: std::sync::Mutex<std::collections::BTreeSet<String>>,
    waited: std::sync::Mutex<std::collections::BTreeSet<String>>,
    recorded: tokio::sync::Notify,
    /// Told whenever a bound plane or the escrow recovery may have changed
    /// the store's unkeyed set, so a read waiting at the unkeyed hold
    /// re-asks the moment it may have cleared.
    unkeyed_changed: tokio::sync::Notify,
}

impl FirstListings {
    /// Has this process recorded `scope` listed since it started?
    pub fn listed(&self, scope: &str) -> bool {
        self.listed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(scope)
    }

    fn note(&self, scope: &str) {
        self.listed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(scope.to_string());
        self.recorded.notify_waiters();
    }

    /// Resolves once this process has recorded `scope` listed.
    pub async fn wait_listed(&self, scope: &str) {
        loop {
            // Registered before the read, so a record between the two is not
            // lost: `notify_waiters` wakes every `Notified` already created.
            let recorded = self.recorded.notified();
            if self.listed(scope) {
                return;
            }
            recorded.await;
        }
    }

    /// Has a read of `scope` already waited out this launch's first pass?
    /// The launch wait is one wait per process and scope, not one per read.
    pub fn launch_waited(&self, scope: &str) -> bool {
        self.waited
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(scope)
    }

    /// Record that a read of `scope` has waited out this launch's first pass.
    pub fn note_launch_waited(&self, scope: &str) {
        self.waited
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(scope.to_string());
    }

    /// The store's unkeyed set may have changed: wake every read waiting at
    /// the unkeyed hold to ask again.
    pub fn note_unkeyed_changed(&self) {
        self.unkeyed_changed.notify_waiters();
    }

    /// Resolves at the next [`Self::note_unkeyed_changed`] after it is
    /// created — create it BEFORE asking, so a change between the question
    /// and the wait is not lost.
    pub fn unkeyed_changed(&self) -> tokio::sync::futures::Notified<'_> {
        self.unkeyed_changed.notified()
    }
}

/// The nest's answer to one [`AccountStatePlane::push_verbatim`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pushed {
    /// Stored: the row is in this pass's listing now.
    Acked,
    /// `stale_writer_seq`: the nest holds or has retired something at or
    /// above the coordinate — the local copy is retired.
    RefusedForGood,
    /// The scope is at its live-entry cap.
    ScopeFull,
}

/// One retire a bound plane sent — an entry of the store's retire record
/// (`account-sync-plane.md` § The bind leg, ruling 5), written by
/// [`AccountStatePlane::retire`] and read by the secondary leg.
pub use fauna_account_store::types::IssuedRetire;

/// A nest's live class-2 rows as one listing: `(writer, item_key)` → the
/// `writer_seq` of the live row the nest holds for that pair (it collapses a
/// writer's rows per item, so there is one).
pub type Listing = BTreeMap<(WriterId, [u8; 32]), u64>;

/// A nest's serve order for the rows of a [`Listing`]: `(writer, item_key)`
/// → the position in that nest's log the live row sits at
/// ([`AccountStatePlane::served_at`]).
pub type ServeOrder = BTreeMap<(WriterId, [u8; 32]), u64>;

/// One attested predecessor's delegable row a full-state reconcile opened
/// under a retired schedule and merged ([`AccountStatePlane::inherited_rows`])
/// — a candidate for the retire behind the carry (`succession-aftermath.md`
/// § Re-key scope → *The predecessor's own row is retired behind the carry*,
/// clause (1); `crate::delegable_reclaim`): its coordinates as served, and
/// the logical item it opened to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InheritedRow {
    /// The predecessor writer.
    pub writer: WriterId,
    /// Its `writer_seq` — the row's own coordinate.
    pub writer_seq: u64,
    /// The blinded item key the retired schedule derived.
    pub item_key: [u8; 32],
    /// The bound nest's serve coordinate, for the gate's watermark.
    pub feed_seq: Option<u64>,
    /// The kind the row opened to.
    pub kind: String,
    /// The logical key the row opened to.
    pub key: String,
}

/// A predecessor's mint record the fleet walk opened under the mint-kind keys
/// and could not carry for want of the key
/// ([`AccountStatePlane::unkeyed_predecessor_mints`]): the record is true —
/// it decodes, binds to its own id and its authorship verifies — and the
/// retained bundle holds no key at that id.
#[derive(Debug, Clone)]
pub struct UnkeyedPredecessorMint {
    /// The generation id — the record's content-derived key.
    pub generation_id: [u8; 32],
    /// Its core, whose key commitment an opened wrap is checked against.
    pub core: fauna_core::generation::MintCore,
}

/// The nest refused a put `scope_full`: the scope is at its live-entry cap.
/// Typed so a caller tells it from every other publish failure through any
/// `context` layered on it ([`is_scope_full`]); the row stays journaled.
#[derive(Debug)]
pub struct ScopeFull {
    /// The scope the put was refused on.
    pub scope: String,
    /// The nest's refusal, as the transport rendered it.
    pub refusal: String,
}

impl std::fmt::Display for ScopeFull {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.scope == ACCOUNT_STATE_SCOPE {
            // Refinement 11 → *A row refused for room is parked*: what makes
            // room on this scope is a conversation being left, which no pass
            // can promise, so the text promises nothing about when.
            write!(
                f,
                "{KIND_STATE_PUT}: {} — the scope is at its live-entry cap; this row is parked \
                 and lands when the scope has room, and the publish goes on past it",
                self.refusal
            )
        } else {
            write!(
                f,
                "{KIND_STATE_PUT}: {} — the scope is at its live-entry cap; this row stays \
                 journaled and publish_pending re-sends it once the fleet scope's reclamation \
                 pass (generation_reclaim) has retired a row, which needs no room",
                self.refusal
            )
        }
    }
}

impl std::error::Error for ScopeFull {}

/// Is `err` a [`ScopeFull`] refusal, under whatever context it carries?
pub fn is_scope_full(err: &anyhow::Error) -> bool {
    err.downcast_ref::<ScopeFull>().is_some()
}

/// The nest refused a put's coordinate for good (`stale_writer_seq`): typed
/// so the delegable plane's parked retry tells it from a transport fault
/// ([`AccountStatePlane::publish_pending`]). The text is the one the publish
/// always answered with.
#[derive(Debug)]
struct CoordinateRefused {
    refusal: String,
}

impl std::fmt::Display for CoordinateRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{KIND_STATE_PUT}: {}", self.refusal)
    }
}

impl std::error::Error for CoordinateRefused {}

/// **The gate's watermark, client side**: is a retire of a row at `feed_seq`
/// one the nest's retention gate would refuse `not_yet_stable`, judged
/// against the watermark `withhold_above`
/// ([`AccountStatePlane::retirable_through_seq`])? A retire it names is
/// withheld without a request, and kept exactly as a deferred one is. An
/// unknown coordinate or watermark withholds nothing (the nest still gates).
/// The one home of the comparison: the fleet scope's reclamation pass and the
/// delegable scope's retire behind the carry both ask it.
pub fn withheld_by_gate(withhold_above: Option<u64>, feed_seq: Option<u64>) -> bool {
    matches!((withhold_above, feed_seq), (Some(watermark), Some(at)) if at > watermark)
}

/// Raise `listing`'s slot for `(writer, item_key)` to `writer_seq`.
fn list(listing: &mut Listing, writer: &WriterId, item_key: &[u8; 32], writer_seq: u64) {
    let slot = listing.entry((*writer, *item_key)).or_insert(writer_seq);
    *slot = (*slot).max(writer_seq);
}

/// [`AccountStatePlane::new`]'s classifier: the nest's typed final refusal of
/// a coordinate ([`fauna_protocol::RpcError::CODE_ACCOUNT_STATE_STALE_WRITER_SEQ`]).
fn stale_writer_seq_refusal<E: fauna_protocol::RpcErrorClass>(e: &E) -> bool {
    e.as_rpc_error()
        .is_some_and(|rpc| rpc.is_account_state_stale_writer_seq())
}

/// [`AccountStatePlane::new_pull_only`]'s classifier: the peer leg never
/// sends, so nothing is ever refused.
fn never_refused<E>(_: &E) -> bool {
    false
}

impl<'a, B: StoreBackend, R: RpcRequester> AccountStatePlane<'a, B, R> {
    /// # Errors
    /// `writer_key`'s public half is not the store's writer id — every entry
    /// this plane sealed would then fail on every reader, so it is refused here
    /// rather than one write at a time.
    ///
    /// The requester's error must classify (`RpcErrorClass`): this is the
    /// nest leg, and a nest's typed final refusal of a coordinate is acted on
    /// at [`Self::publish`] (see [`Self::refused_for_good`]).
    pub fn new(
        store: &'a AccountStore<B>,
        rpc: &'a R,
        schedule: &'a AccountStateKeySchedule,
        writer_key: &'a SigningKey,
        trust: &'a GenerationTrust,
        scope: impl Into<String>,
    ) -> Result<Self>
    where
        R::Error: fauna_protocol::RpcErrorClass,
    {
        Self::build(
            store,
            rpc,
            schedule,
            writer_key,
            trust,
            scope,
            true,
            stale_writer_seq_refusal::<R::Error>,
        )
    }

    /// The **linked-nest** constructor (`account-sync-plane.md` § The bind
    /// leg, ruling 4): `rpc` is an owner-authenticated connection to a nest
    /// the user linked with the `account_replica` capability, whose channel
    /// binding the caller has already checked against the pairing row
    /// (`crate::linked_leg`). See [`Self::linked`] for what such a plane
    /// leaves to the bound one.
    ///
    /// # Errors
    /// As [`Self::new`].
    pub fn new_linked(
        store: &'a AccountStore<B>,
        rpc: &'a R,
        schedule: &'a AccountStateKeySchedule,
        writer_key: &'a SigningKey,
        trust: &'a GenerationTrust,
        scope: impl Into<String>,
    ) -> Result<Self>
    where
        R::Error: fauna_protocol::RpcErrorClass,
    {
        let mut plane = Self::new(store, rpc, schedule, writer_key, trust, scope)?;
        plane.linked = true;
        Ok(plane)
    }

    /// The **peer-leg** constructor (W2.6, charter § The peer leg): `rpc` is a
    /// peer-channel requester serving only the sync-transfer kinds, so this
    /// plane walks and reconciles but never publishes — see
    /// [`Self::publish_to_feed`]. Local writes through [`Self::put`] still
    /// land (local-first is the plane's law); they reach the nest via the
    /// nest leg's [`Self::publish_pending`].
    pub fn new_pull_only(
        store: &'a AccountStore<B>,
        rpc: &'a R,
        schedule: &'a AccountStateKeySchedule,
        writer_key: &'a SigningKey,
        trust: &'a GenerationTrust,
        scope: impl Into<String>,
    ) -> Result<Self> {
        Self::build(
            store,
            rpc,
            schedule,
            writer_key,
            trust,
            scope,
            false,
            never_refused::<R::Error>,
        )
    }

    /// The **throwaway replica's** constructor
    /// (`crate::cold_replica`): a pull-only plane that also never seals what
    /// it authors — see [`Self::fold_only`]. A walk's merge lands in the
    /// replica's store, so its reads fold every writer's rows as any replica
    /// does, and nothing it holds ever leaves it.
    pub fn new_fold_only(
        store: &'a AccountStore<B>,
        rpc: &'a R,
        schedule: &'a AccountStateKeySchedule,
        writer_key: &'a SigningKey,
        trust: &'a GenerationTrust,
        scope: impl Into<String>,
    ) -> Result<Self> {
        let mut plane = Self::new_pull_only(store, rpc, schedule, writer_key, trust, scope)?;
        plane.fold_only = true;
        Ok(plane)
    }

    #[allow(clippy::too_many_arguments)] // the two constructors' one seam
    fn build(
        store: &'a AccountStore<B>,
        rpc: &'a R,
        schedule: &'a AccountStateKeySchedule,
        writer_key: &'a SigningKey,
        trust: &'a GenerationTrust,
        scope: impl Into<String>,
        publish_to_feed: bool,
        refused_for_good: fn(&R::Error) -> bool,
    ) -> Result<Self> {
        if writer_key.verifying_key().to_bytes() != store.writer().0 {
            bail!(
                "account-state plane: the signing key is not this store's writer — every entry \
                 it sealed would fail the R13 writer-signature check on every reading replica"
            );
        }
        Ok(Self {
            store,
            rpc,
            schedule,
            writer_key,
            trust,
            scope: scope.into(),
            publish_to_feed,
            linked: false,
            fold_only: false,
            mint_lock: tokio::sync::Mutex::new(()),
            custody: None,
            predecessors: &[],
            predecessor_mint_keys: &[],
            predecessor_machinery_keys: &[],
            machinery_hint: std::sync::Mutex::new(None),
            machinery_misses: std::sync::Mutex::new(std::collections::BTreeSet::new()),
            refused_for_good,
            trial_hint: std::sync::Mutex::new(None),
            retirable_through: std::sync::Mutex::new(None),
            listing: std::sync::Mutex::new(None),
            collecting: std::sync::Mutex::new(None),
            served: std::sync::Mutex::new(None),
            collecting_served: std::sync::Mutex::new(None),
            carry_names: std::sync::Mutex::new(BTreeMap::new()),
            inherited_rows: std::sync::Mutex::new(None),
            collecting_inherited: std::sync::Mutex::new(None),
            unkeyed_predecessor_mints: std::sync::Mutex::new(None),
            collecting_unkeyed_predecessor_mints: std::sync::Mutex::new(None),
            first_listings: None,
            listing_owed: std::sync::atomic::AtomicBool::new(false),
            admitted: None,
            ext_writers: std::sync::Mutex::new(None),
        })
    }

    /// Is this the **bound** nest's plane — the one whose log the watermark,
    /// the relay rows' serve coordinates and our own published high-water
    /// name? `false` on the peer leg and on a linked nest's plane
    /// ([`Self::linked`]).
    fn bound(&self) -> bool {
        self.publish_to_feed && !self.linked
    }

    /// Is this a linked nest's plane ([`Self::new_linked`])?
    pub fn is_linked(&self) -> bool {
        self.linked
    }

    /// The serve coordinate a row this plane was served carries into the
    /// relay plane: the bound nest's own, and never a linked nest's — a
    /// relay row's `feed_seq` is a position in the bound nest's log, which
    /// the reclamation pass compares with that nest's gate.
    fn feed_coordinate(&self, served_seq: i64) -> Option<u64> {
        if self.linked {
            None
        } else {
            u64::try_from(served_seq).ok()
        }
    }

    /// A position in THIS plane's nest's log, a linked nest's included —
    /// the serve order's ([`Self::served_at`]), never a relay row's.
    fn feed_coordinate_of(&self, served_seq: i64) -> Option<u64> {
        u64::try_from(served_seq).ok()
    }

    /// This pass's listing ([`Self::listing`]), or `None` when no full-state
    /// reconcile has completed against the bound nest since the last began.
    pub fn listing(&self) -> Option<Listing> {
        self.listing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// This pass's inherited rows ([`Self::inherited_rows`]), or `None` when
    /// no full-state reconcile has completed since the last began.
    pub fn inherited_rows(&self) -> Option<Vec<InheritedRow>> {
        self.inherited_rows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// This pass's unkeyed predecessor mints
    /// ([`Self::unkeyed_predecessor_mints`]), or `None` when no full-state
    /// reconcile has completed since the last began.
    pub fn unkeyed_predecessor_mints(&self) -> Option<Vec<UnkeyedPredecessorMint>> {
        self.unkeyed_predecessor_mints
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// A predecessor mint record the walk in flight declined for want of the
    /// key — collected when that walk is a full-state reconcile.
    fn note_unkeyed_predecessor_mint(&self, mint: UnkeyedPredecessorMint) {
        if let Some(mints) = self
            .collecting_unkeyed_predecessor_mints
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_mut()
            && !mints.iter().any(|m| m.generation_id == mint.generation_id)
        {
            mints.push(mint);
        }
    }

    /// Stand in for a completed reconcile's inherited rows, as
    /// [`Self::set_listing`] does for its listing.
    #[cfg(test)]
    pub(crate) fn set_inherited_rows(&self, rows: Option<Vec<InheritedRow>>) {
        *self
            .inherited_rows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = rows;
    }

    /// Was this plane handed any predecessor's delegable schedule
    /// ([`Self::with_predecessor_schedules`])? Only such a plane's walk opens
    /// a row under a retired schedule.
    pub fn holds_predecessor_schedules(&self) -> bool {
        !self.predecessors.is_empty()
    }

    /// A row the walk in flight carried under a predecessor's schedule —
    /// collected when that walk is a full-state reconcile.
    fn note_inherited(&self, row: Option<InheritedRow>) {
        let Some(row) = row else { return };
        if let Some(rows) = self
            .collecting_inherited
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_mut()
        {
            rows.push(row);
        }
    }

    /// Stand in for a completed reconcile's listing — the nest's live rows as
    /// a tier-1 test states them, where a stub feed serves none.
    #[cfg(test)]
    pub(crate) fn set_listing(&self, listing: Option<Listing>) {
        *self
            .listing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = listing;
    }

    /// **"Published" as this pass saw it** (`account-sync-plane.md` § The bind
    /// leg, ruling 1(d)): the bound nest holds `writer`'s row for `item_key`
    /// at `writer_seq` or later — in this pass's listing, or acked since.
    /// `false` whenever there is no listing: a frontier slot says only that
    /// SOME nest once acked a row, which is no evidence about this one.
    pub fn listed_at_or_above(
        &self,
        writer: &WriterId,
        item_key: &[u8; 32],
        writer_seq: u64,
    ) -> bool {
        self.listing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(|l| l.get(&(*writer, *item_key)))
            .is_some_and(|held| *held >= writer_seq)
    }

    /// The nest acked `writer`'s row at `(item_key, writer_seq)`: this pass has
    /// seen it published. A no-op with no listing — an ack alone is not a
    /// listing, and would read every row it does not name as missing.
    fn note_acked(
        &self,
        writer: &WriterId,
        item_key: &[u8; 32],
        writer_seq: u64,
        served: Option<u64>,
    ) {
        if let Some(listing) = self
            .listing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_mut()
        {
            list(listing, writer, item_key, writer_seq);
            let mut order = self
                .served
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let order = order.get_or_insert_with(BTreeMap::new);
            match served {
                Some(at) => {
                    order.insert((*writer, *item_key), at);
                }
                // A position this put did not name is unknown, never the
                // position of the row it collapsed.
                None => {
                    order.remove(&(*writer, *item_key));
                }
            }
        }
    }

    /// A row the walk in flight was served — collected when that walk is a
    /// full-state reconcile.
    fn note_listed(
        &self,
        writer: &WriterId,
        item_key: &[u8; 32],
        writer_seq: u64,
        served: Option<u64>,
    ) {
        if let Some(listing) = self
            .collecting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_mut()
        {
            list(listing, writer, item_key, writer_seq);
            if let (Some(order), Some(at)) = (
                self.collecting_served
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_mut(),
                served,
            ) && listing.get(&(*writer, *item_key)) == Some(&writer_seq)
            {
                order.insert((*writer, *item_key), at);
            }
        }
    }

    /// **Where this plane's nest serves `writer`'s live row for `item_key`**,
    /// as this pass saw it ([`Self::served`]): `None` when the row is not in
    /// the listing, there is no listing, or the position is unknown. The
    /// serve order *below* compares (`crate::delegable_reclaim::below`).
    pub fn served_at(&self, writer: &WriterId, item_key: &[u8; 32]) -> Option<u64> {
        self.served
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(|order| order.get(&(*writer, *item_key)).copied())
    }

    /// The serve position of `row` at this plane's nest: [`Self::served_at`]
    /// when the listing holds the row at its own `writer_seq`; otherwise, on
    /// the bound nest only, the relay row's own `feed_seq` (a position in the
    /// same log, stamped when the row was served or acked). A linked nest's
    /// position is its listing's alone.
    pub fn serve_position(&self, row: &RelayRow) -> Option<u64> {
        let item_key = <[u8; 32]>::try_from(row.item_key.as_slice()).ok()?;
        let listed = self
            .listing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(|l| l.get(&(row.writer, item_key)).copied());
        if listed == Some(row.writer_seq)
            && let Some(at) = self.served_at(&row.writer, &item_key)
        {
            return Some(at);
        }
        if self.bound() { row.feed_seq } else { None }
    }

    /// Stand in for a completed reconcile's serve order, as
    /// [`Self::set_listing`] does for its listing.
    #[cfg(test)]
    pub(crate) fn set_served(&self, order: Option<ServeOrder>) {
        *self
            .served
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = order;
    }

    /// The feed's retention-gate watermark as this plane's last walk page
    /// read it ([`Self::retirable_through`]) — what the reclamation pass
    /// compares a relay row's `feed_seq` against to withhold a retire the
    /// gate would refuse (`account-data-taxonomy.md` § The generation
    /// machinery → *Fleet-scope reclamation*, clause (1) → *the gate's
    /// watermark*). `None` withholds nothing.
    pub fn retirable_through_seq(&self) -> Option<u64> {
        *self
            .retirable_through
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Bank a walk page's watermark. A reply that carries none (a current
    /// nest that answers no mark, the peer leg's store-served page) clears it: a value banked
    /// from a nest that has stopped serving it must not outlive the walk
    /// that learned so, or a rolled-back nest would keep withholding.
    fn note_retirable_through(&self, echoed: Option<i64>) {
        *self
            .retirable_through
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            echoed.and_then(|seq| u64::try_from(seq).ok());
    }

    /// Attach the retained-bundle custody (the runtime's `PrincipalSlot`) —
    /// builder-style so the many existing construction sites stay untouched.
    pub fn with_generation_custody(
        mut self,
        custody: &'a dyn crate::generation_tip::RetainedKeyCustody,
    ) -> Self {
        self.custody = Some(custody);
        self
    }

    /// Hand the walk its attested predecessors' generation-0 delegable
    /// schedules (`succession-aftermath.md` § Re-key scope): a walked v1 row
    /// that does not open under this plane's own schedule is tried under each,
    /// and one that opens is carried ([`WalkReport::inherited`]).
    ///
    /// The driver sets it on the **bound nest's** delegable-scope plane only.
    /// The peer leg's planes and a linked nest's are handed none, by rule
    /// (the same section, *Which walks carry*): a retired key is tried only
    /// on what the home nest serves, whose write door refused the retired
    /// identity at the ceremony. The parameter
    /// type is what keeps the carry off the fleet branch whatever plane it is
    /// set on: a [`DelegableSchedule`] derives no fleet-only key, so a
    /// predecessor's fleet-only generation-0 row stays unopened.
    pub fn with_predecessor_schedules(mut self, predecessors: &'a [DelegableSchedule]) -> Self {
        self.predecessors = predecessors;
        self
    }

    /// Hand the walk its attested predecessors' generation-0 keys for the
    /// mint kind (`owner-key-material.md` § Path A-sibling-2 → *Rotation*,
    /// the succession rider → *What crosses*): a walked v1 row that does not
    /// open under this plane's own schedule is tried under each, and one that
    /// opens is carried ([`WalkReport::inherited`]) when it is the true mint
    /// record of a generation whose key this device already holds — the
    /// predicate [`Self::trial_open_predecessor_mint`] owns. Any other row
    /// stays [`WalkReport::unopened`].
    ///
    /// The driver sets it on the **bound nest's** fleet-scope plane only, for
    /// the reasons the delegable carry's schedules are (*Which walks carry*).
    /// The parameter type is what keeps the carry to the one kind: an
    /// [`AccountStateKindKeys`] opens its own kind's rows and no other's, so
    /// a predecessor's device-set, wrap, escrow-target, escrow-receipt,
    /// unkeyable and reach rows stay unopened whatever plane it is set on.
    pub fn with_predecessor_mint_keys(mut self, keys: &'a [AccountStateKindKeys]) -> Self {
        self.predecessor_mint_keys = keys;
        self
    }

    /// Hand the reclamation pass its attested predecessors' generation-0 keys
    /// for every fleet-only machinery kind
    /// (`AttestedPredecessors::retired_machinery_keys`;
    /// `account-data-taxonomy.md` § The generation machinery → *Fleet-scope
    /// reclamation*, clause (3)(i); `owner-key-material.md` § Path
    /// A-sibling-2 → *Rotation*, *The retired machinery keys open only to
    /// retire*). Only [`Self::open_predecessor_machinery_row`] tries them:
    /// the walk keeps [`Self::with_predecessor_mint_keys`]' one kind, and
    /// [`Self::open_relay_row`] — whose answer the publish diff vouches —
    /// keeps the plane's own keys.
    ///
    /// The driver sets it on the **bound nest's** fleet-scope plane only, for
    /// the reasons the carries' keys are (`succession-aftermath.md` § Re-key
    /// scope → *Which walks carry*): a retired key is tried only on what the
    /// home nest serves.
    pub fn with_predecessor_machinery_keys(mut self, keys: &'a [AccountStateKindKeys]) -> Self {
        self.predecessor_machinery_keys = keys;
        self
    }

    /// Was this plane handed any retired machinery keys
    /// ([`Self::with_predecessor_machinery_keys`])? The predecessor arm is
    /// skipped outright when not.
    pub fn has_predecessor_machinery_keys(&self) -> bool {
        !self.predecessor_machinery_keys.is_empty()
    }

    /// Open one of this replica's relay rows under the retired machinery
    /// keys ([`Self::with_predecessor_machinery_keys`]) — clause (3)(i)'s
    /// open, used by the reclamation pass's predecessor arm alone, to learn
    /// that a row is a retired identity's machinery and of which kind, and
    /// then retire it. Form v1 only (the machinery kinds are `Gen0`, and v1
    /// IS the gen-0 form). `None` for a v2 row, a malformed one, and a row no
    /// retired pair opens (remembered, and not tried again while this plane
    /// lives). It applies none of [`Self::trial_open_predecessor_mint`]'s
    /// carry conditions, because what it opens is never merged, re-authored,
    /// vouched to the publish diff or pushed: the caller retires the row or
    /// leaves it.
    pub fn open_predecessor_machinery_row(&self, row: &RelayRow) -> Option<EntryPlaintext> {
        let keys = self.predecessor_machinery_keys;
        if keys.is_empty() {
            return None;
        }
        let envelope = row.entry.as_deref()?;
        if peek_generation_id(envelope).is_some() {
            return None;
        }
        let item_key = <[u8; 32]>::try_from(row.item_key.as_slice()).ok()?;
        let memo = (row.writer.0, row.writer_seq, item_key);
        if self
            .machinery_misses
            .lock()
            .is_ok_and(|misses| misses.contains(&memo))
        {
            return None;
        }
        let coords = EntryCoordinates {
            writer_id: row.writer.0,
            writer_seq: row.writer_seq,
            scope: &self.scope,
        };
        // The pair that opened the previous row first, then the rest — pure
        // ordering, as in [`Self::trial_kinds`]. `open_entry` checks the
        // sealed kind against the pair's own, so what opens names its kind.
        let hint = self.machinery_hint.lock().ok().and_then(|h| *h);
        let order = hint
            .filter(|at| *at < keys.len())
            .into_iter()
            .chain((0..keys.len()).filter(|at| Some(*at) != hint));
        for at in order {
            if let Ok(plaintext) = open_entry(&keys[at], &coords, &item_key, envelope) {
                if let Ok(mut h) = self.machinery_hint.lock() {
                    *h = Some(at);
                }
                return Some(plaintext);
            }
        }
        if let Ok(mut misses) = self.machinery_misses.lock() {
            misses.insert(memo);
        }
        None
    }

    /// Hand the plane the runtime's [`FirstListings`], so a read waiting at
    /// the first-listing gate returns the moment this plane records its scope
    /// listed. The driver sets it on its two bound planes.
    pub fn with_first_listings(mut self, first_listings: &'a FirstListings) -> Self {
        self.first_listings = Some(first_listings);
        self
    }

    /// Hand the plane the account's admitted-kinds overlay, so it writes,
    /// opens and merges the `ext.*` kinds a verified manifest admitted
    /// (`third-party-kinds.md` § The kinds vocabulary). An `ext:<kind>` plane
    /// needs it: without one its kind is "not on the plane here" — rows stay
    /// unopened and a write is refused, the compat answer.
    pub fn with_admitted_kinds(mut self, admitted: &'a AdmittedKinds) -> Self {
        self.admitted = Some(admitted);
        self
    }

    /// The `ext:<kind>` plane beside this one, for one admitted kind
    /// (`third-party-kinds.md` § The `ext` sub-scope): the same store,
    /// connection, schedule, writer key and trust, the scope `ext:<kind>`,
    /// and `admitted` as its overlay. The account driver derives one per
    /// admitted kind from its bound fleet plane at every pass. An `ext.*`
    /// kind is `Gen0` on the delegable rung, so none of this plane's
    /// generation custody, predecessor keys or first-listing gate carries
    /// over: the new plane is plane-native in all of them.
    pub fn ext_plane<'p>(
        &self,
        kind: &fauna_protocol::ext_kind::ExtKind,
        admitted: &'p AdmittedKinds,
    ) -> AccountStatePlane<'p, B, R>
    where
        'a: 'p,
    {
        let mut plane = AccountStatePlane::build(
            self.store,
            self.rpc,
            self.schedule,
            self.writer_key,
            self.trust,
            fauna_protocol::scope::ext_scope(kind),
            self.publish_to_feed,
            self.refused_for_good,
        )
        .expect("the writer key was checked against this store when this plane was built");
        plane.linked = self.linked;
        plane.with_admitted_kinds(admitted)
    }

    /// The overlay every registry lookup of this plane consults.
    fn kinds(&self) -> &AdmittedKinds {
        self.admitted.unwrap_or(&NO_ADMITTED_KINDS)
    }

    /// The `ext.*` kind this plane's scope names, when it is an `ext:<kind>`
    /// plane — whose one kind IS the trial set (a feed of one kind needs no
    /// trial).
    fn ext_kind(&self) -> Option<String> {
        fauna_protocol::scope::ext_scope_kind(&self.scope).map(|k| k.to_string())
    }

    /// A listing of this scope ran to its end with `report`: record the
    /// scope **listed** (`account-client-lifecycle.md` § The client-side
    /// lifecycle → *The first listing*, clause (1)) at once when it left no
    /// row unopened, otherwise owe it to the end of the pass
    /// ([`Self::pass_ended`]). The bound nest's plane only: a peer or a
    /// linked nest is not held complete, so its listing records nothing.
    async fn listing_ran(&self, report: &WalkReport, full: bool) -> Result<()> {
        if !self.bound() {
            return Ok(());
        }
        // The unkeyed set (clause (5), *The fact*): a full listing replaces it
        // with its own, a nudge's walk only adds. Only a scope that holds
        // generation-sealed rows ever names one, so the delegable scope's
        // set stays empty and unwritten.
        if !report.unkeyed.is_empty() {
            tracing::info!(
                scope = %self.scope,
                full,
                unkeyed = report.unkeyed.len(),
                unopened = report.unopened,
                "listing left rows unopened under generations this device does not key"
            );
        }
        if full {
            self.store
                .replace_unkeyed(&self.scope, &report.unkeyed)
                .await?;
        } else if !report.unkeyed.is_empty() {
            self.store.add_unkeyed(&self.scope, &report.unkeyed).await?;
        }
        if let Some(first_listings) = self.first_listings {
            first_listings.note_unkeyed_changed();
        }
        if report.unopened == 0 {
            self.record_listed().await
        } else {
            self.listing_owed
                .store(true, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        }
    }

    /// The pass (or the nudge's walk) that ran this plane's listing has
    /// ended: record the scope listed if a listing of it ran to its end and
    /// was held back only by rows it could not open. A row this device can
    /// open is open by now — the pass re-presents what its escrow recovery
    /// keyed — and one it still cannot must not refuse every read for ever.
    /// Nothing to do when no listing ran, or it recorded the fact itself.
    pub async fn pass_ended(&self) -> Result<()> {
        if self
            .listing_owed
            .swap(false, std::sync::atomic::Ordering::Relaxed)
        {
            self.record_listed().await?;
        }
        Ok(())
    }

    async fn record_listed(&self) -> Result<()> {
        self.listing_owed
            .store(false, std::sync::atomic::Ordering::Relaxed);
        if self.first_listings.is_some_and(|l| l.listed(&self.scope)) {
            return Ok(());
        }
        if !self.store.listed(&self.scope).await? {
            self.store.record_listed(&self.scope).await?;
        }
        if let Some(first_listings) = self.first_listings {
            first_listings.note(&self.scope);
        }
        Ok(())
    }

    pub fn scope(&self) -> &str {
        &self.scope
    }

    /// The attached retained-bundle custody, for sibling steps that resolve
    /// keys off the same store (`device_endpoints_writer`, `generation_topup`).
    ///
    /// It carried a `cfg_attr(not(feature = "account-runtime"), expect(dead_code))`
    /// from W5.4a — landed ahead of its only caller, then live only in a
    /// feature build — and that conditional expectation went unfulfilled twice,
    /// redding the workspace clippy gate both times. It is gone because the
    /// condition is: `generation_topup` (2026-08-15, W5.8) calls this
    /// unconditionally, so the accessor is live in every build.
    pub fn generation_custody(&self) -> Option<&'a dyn crate::generation_tip::RetainedKeyCustody> {
        self.custody
    }

    /// The store and identity line this plane writes against, for a sibling
    /// step that must re-read merged state at its own write rather than
    /// trust a view taken before one of the pass's yields —
    /// `generation_topup::put_heal`'s removal re-check.
    pub fn store_and_trust(&self) -> (&'a AccountStore<B>, &'a GenerationTrust) {
        (self.store, self.trust)
    }

    /// The runtime's [`FirstListings`] this plane tells, if any — for the
    /// escrow recovery, whose answered-empty bit a waiting read must hear.
    pub fn first_listings(&self) -> Option<&'a FirstListings> {
        self.first_listings
    }

    /// The plane's own requester, for the one sibling step that asks the nest
    /// something the plane does not (`generation_escrow_recover` — the escrow
    /// door rides the same session the mint's deposit does).
    pub fn requester(&self) -> &'a R {
        self.rpc
    }

    // ── Publish ─────────────────────────────────────────────────────────────

    /// Write one class-2 value locally and publish it to the scope's feed.
    ///
    /// Local first, always: the store assigns the writer seq the envelope's AAD
    /// binds, and a user's write must survive a failed network call. Returns
    /// the writer seq the entry landed on; a publish failure is returned as an
    /// error *after* the local write is durable, and
    /// [`Self::publish_pending`] is what re-sends it.
    pub async fn put(
        &self,
        item: &ItemId,
        value: Vec<u8>,
        merge_meta: Option<Vec<u8>>,
    ) -> Result<u64> {
        let (plaintext, seq) = self.write_local(item, value, merge_meta, false).await?;
        self.publish_own_rows_through(&plaintext, seq).await?;
        Ok(seq)
    }

    /// [`Self::put`] naming the rows the new row covers — `replaces`
    /// (`delegable-scope-reclamation.md` § Delegable-scope reclamation, part
    /// (2)): the nest supersedes each that is live at exactly its coordinates
    /// in the put's own transaction, before it counts the cap, so the put
    /// needs no free pair for them. Which rows to name is the caller's: rows
    /// of this scope that it has opened and merged and that are below the
    /// row it sends. Once the put lands, the relay copy of every named row is
    /// forgotten.
    ///
    /// The ordering law is [`Self::put`]'s: every unsent own row ahead of
    /// this one is drained first, and a drain that fails holds this row back
    /// (its publish error is returned after the local write is durable). The
    /// list rides this send only: a row a later [`Self::publish_pending`]
    /// re-sends names nothing, and what it would have named is left to the
    /// pass's retires. The peer leg sends nothing, so it names nothing.
    pub async fn put_replacing(
        &self,
        item: &ItemId,
        value: Vec<u8>,
        merge_meta: Option<Vec<u8>>,
        replaces: &[RelayRow],
    ) -> Result<u64> {
        self.replaces_on_wire(replaces)?;
        let (plaintext, seq) = self.write_local(item, value, merge_meta, false).await?;
        if self.bound() {
            self.publish_pending_below(Some(seq))
                .await
                .context("publishing this replica's own rows in journal order")?;
        }
        self.publish(&plaintext, seq, replaces).await?;
        Ok(seq)
    }

    /// [`Self::put`] for a writer's **last word** — the sign-out severance's
    /// own `Removed` row (`account-data-taxonomy.md` § The generation
    /// machinery → *Fleet-scope reclamation*, clause (4)), written while the
    /// runtime still holds the writer key the sign-out is about to erase.
    ///
    /// The ordered own publish drains every unsent own row first, as
    /// [`Self::put`] does; unlike it, a drain that fails does not hold this
    /// row back — it is sent at its own coordinate anyway, and the published
    /// slot moves past whatever the drain left. The contiguous-prefix law
    /// (refinement 11 → *the ordered own publish*) guards a slot a later
    /// pass or a heal goes on to read, and a dying writer has neither: the
    /// sign-out erases the journal and the key right after this. What it
    /// does have is the case the law would otherwise wedge — a sign-out cut
    /// the pass inside a put the nest recorded, the reply died with the
    /// dropped future, and the drain's re-send of that row is refused as the
    /// replay it is (`stale_writer_seq`); nothing is lost there (the nest
    /// holds the row), while a `Removed` row queued behind it would leave the
    /// device un-severed on the fleet plane. An error here is this row's own
    /// publish failing.
    pub async fn put_last_word(
        &self,
        item: &ItemId,
        value: Vec<u8>,
        merge_meta: Option<Vec<u8>>,
    ) -> Result<u64> {
        let (plaintext, seq) = self.write_local(item, value, merge_meta, false).await?;
        if self.bound()
            && let Err(e) = self.publish_pending_below(Some(seq)).await
        {
            tracing::warn!(
                scope = %self.scope,
                seq,
                "a writer's last word: the own rows ahead of it did not all publish \
                 ({e:#}) — it is sent at its own coordinate regardless, since the \
                 journal and key it would wait on die with this sign-out"
            );
        }
        self.publish(&plaintext, seq, &[]).await?;
        Ok(seq)
    }

    /// [`Self::put`]'s local half alone: the row is durable, sealed-to-be
    /// and stamped with its writer seq, and **nothing is sent**. The publish
    /// is [`Self::publish_pending`]'s on the pump's next publish step — the
    /// account runtime's local-write wake (`account-data-plane.md` § The
    /// client-side lifecycle, the pump bullet → wake source (4)), which is
    /// what lets a preference write answer while a pass is in flight instead
    /// of parking behind it. Same preflight, same refusals, same ordering law
    /// once it ships (every unsent own row before its own, in journal order).
    pub async fn put_local(
        &self,
        item: &ItemId,
        value: Vec<u8>,
        merge_meta: Option<Vec<u8>>,
    ) -> Result<u64> {
        Ok(self.write_local(item, value, merge_meta, false).await?.1)
    }

    /// Tombstone one class-2 item — an ordinary sealed entry carrying the
    /// tombstone marker (§ The class-2 entry form: "a relay cannot fabricate a
    /// tombstone that opens"), never a bare cleartext row.
    ///
    /// **Only for kinds whose policy admits one** (`MergePolicy::
    /// admits_tombstone`); this refuses otherwise, see
    /// [`Self::write_local_and_publish`]. `merge_meta` carries the stamp on a
    /// stamped policy — a deletion with no stamp has nothing ordering it
    /// against a concurrent write, so the door requires it rather than letting
    /// the reader discover the omission.
    pub async fn tombstone(&self, item: &ItemId, merge_meta: Option<Vec<u8>>) -> Result<u64> {
        let (plaintext, seq) = self.write_local(item, Vec::new(), merge_meta, true).await?;
        self.publish_own_rows_through(&plaintext, seq).await?;
        Ok(seq)
    }

    /// [`Self::tombstone`]'s local half alone — [`Self::put_local`]'s twin
    /// for a deletion: the tombstone is durable and stamped, **nothing is
    /// sent**, and the account runtime's publish step ships it. Same policy
    /// door (only a kind whose policy admits a tombstone).
    pub async fn tombstone_local(&self, item: &ItemId, merge_meta: Option<Vec<u8>>) -> Result<u64> {
        Ok(self
            .write_local(item, Vec::new(), merge_meta, true)
            .await?
            .1)
    }

    /// The local half every write shares: preflight, admission, the durable
    /// journal row. Returns the plaintext the publish leg seals and the
    /// writer seq it landed on.
    async fn write_local(
        &self,
        item: &ItemId,
        value: Vec<u8>,
        merge_meta: Option<Vec<u8>>,
        tombstone: bool,
    ) -> Result<(EntryPlaintext, u64)> {
        let Some(policy) = self.kinds().merge_policy(&item.kind) else {
            bail!(
                "kind {:?} is not on the class-2 plane in this build — register it in \
                 fauna_protocol::merge_policy before writing entries under it",
                item.kind
            );
        };
        // The writer door for tombstones. Which policies admit one is part of
        // the policy contract (`MergePolicy::admits_tombstone`,
        // `account-data-plane.md` § Merge-policy seam) — and it is enforced
        // HERE because the reader has no cheap answer left. A tombstone on a
        // `CrdtPerField` kind is unmergeable by design, and the nest collapses
        // per `(item_key, writer)`, so once published the row stays live until
        // this same writer supersedes that item: every other replica meets it
        // on every walk AND every reconcile. This replica would never notice —
        // it does not merge its own rows — which is exactly how a
        // "reset my settings" affordance ships a plane-wide wedge.
        if tombstone && !policy.admits_tombstone() {
            bail!(
                "kind {:?} merges under {:?}, which does not admit a tombstone — no reading \
                 replica can merge one, and publishing it would leave every OTHER device \
                 meeting an unmergeable row (see MergePolicy::admits_tombstone for the \
                 per-policy reasons; per-field or per-item deletion for this policy is a \
                 design slice, not something to express with a tombstone)",
                item.kind,
                policy
            );
        }
        // A stamped policy's deletion is ordered only by its stamp; without one
        // the reader refuses the row loudly and this write is simply lost.
        if policy.requires_stamp() && merge_meta.is_none() {
            bail!(
                "kind {:?} merges under {:?}, which orders values by their LwwStamp — this \
                 write carries no merge_meta, so no reading replica could rank it",
                item.kind,
                policy
            );
        }
        // The per-entry cap, before any durable state — see
        // `refuse_if_over_entry_cap` for why a row no nest accepts must never
        // reach the local journal. The door is passed HERE and its ticket spent
        // below because `admit_origination` sits between the two and can mint a
        // generation: an entry that can never publish must not spend one, and
        // must keep its own voice rather than answer with the tip's refusal.
        let plaintext = EntryPlaintext {
            kind: item.kind.clone(),
            key: item.key.clone(),
            merge_meta: merge_meta.map(Into::into),
            value: value.into(),
            tombstone,
        };
        let sized = SizedEntry::size(&plaintext)?;
        // The R14 admission, LAST of the preflight checks so a policy-specific
        // complaint (a tombstone the reader could not merge, a missing stamp,
        // an entry over the cap) still reaches the caller as itself rather
        // than as this refusal.
        self.admit_origination(&item.kind).await?;
        let seq = self
            .put_own_row(&sized)
            .await
            .context("account-state put: local write")?;
        Ok((plaintext, seq))
    }

    /// Publish this replica's own rows **in journal order**, up to and
    /// including the row just written at `seq` — the one publish path every
    /// local write takes on the nest leg (`account-replica-posture.md` § The
    /// store device principal, refinement 11 → *the ordered own publish*).
    ///
    /// Why not just seal and send `seq`: our own frontier slot is the
    /// published high-water and MAX-merge, and [`Self::publish_pending`]
    /// scans above it. An inline send accepted while an earlier own row was
    /// still unsent — offline puts, then a put racing the reconnect pass,
    /// which the pump serves queued commands ahead of — raised the slot past
    /// the unsent row, and nothing ever sent it: stranded from the fleet
    /// silently, and, under a burnt journal, exactly the refused-but-below-
    /// the-slot row refinement 11's residue (i) named. Draining in order
    /// keeps the slot the contiguous attempted prefix, so a refused row
    /// wedges exactly there and the heal's re-author bound covers every row
    /// after it. A refused or unreachable earlier row therefore fails THIS
    /// write's publish leg too; the local row is durable either way, and the
    /// next pass retries.
    ///
    /// The peer leg has no publish: it records the relay row for `seq`
    /// alone, as before.
    ///
    /// A fold-only plane ([`Self::fold_only`]) stops at the local row: it
    /// seals nothing.
    async fn publish_own_rows_through(&self, plaintext: &EntryPlaintext, seq: u64) -> Result<()> {
        if self.fold_only {
            return Ok(());
        }
        if !self.bound() {
            return self.publish(plaintext, seq, &[]).await;
        }
        self.publish_pending()
            .await
            .map(drop)
            .context("publishing this replica's own rows in journal order")
    }

    /// Seal `plaintext` at `(our writer, writer_seq)` and send it, then account
    /// our own frontier slot.
    ///
    /// **Our own slot in the scope's frontier is the published high-water.**
    /// That is what makes [`Self::publish_pending`] able to find the rows a
    /// crash or a dropped connection left local-only, and what stops the walk
    /// from paging our own rows back at us.
    ///
    /// `replaces` names the rows this one covers ([`Self::replaces_on_wire`]);
    /// every caller that names none passes an empty slice.
    ///
    /// Boxed once here, so every write's future holds a pointer to it, not
    /// its frame (`native-async-execution.md` § The rule).
    async fn publish(
        &self,
        plaintext: &EntryPlaintext,
        writer_seq: u64,
        replaces: &[RelayRow],
    ) -> Result<()> {
        Box::pin(self.publish_unboxed(plaintext, writer_seq, replaces)).await
    }

    async fn publish_unboxed(
        &self,
        plaintext: &EntryPlaintext,
        writer_seq: u64,
        replaces: &[RelayRow],
    ) -> Result<()> {
        let writer = self.store.writer();
        let sealed = self.seal_for_kind(plaintext, writer_seq).await?;
        self.replaces_on_wire(replaces)?;

        let op = if plaintext.tombstone {
            OP_TOMBSTONE.to_string()
        } else {
            OP_STATE_PUT.to_string()
        };

        // The relay plane (W2.6): our own live row, servable to a peer
        // verbatim — recorded BEFORE the network call, because a row a nest
        // outage keeps un-published is exactly the row the peer leg exists to
        // carry. Idempotent under `publish_pending`'s re-seal (same
        // coordinates → the store's newer-seq guard makes it a no-op). The
        // law is about OUTAGES: a row the nest refuses FOR GOOD is retired
        // again below (refinement 11 → *a refused row's relay residue*).
        self.store
            .record_relay_row(&RelayRow {
                scope: self.scope.clone(),
                item_class: ItemClass::StateEntry.as_wire().to_string(),
                writer,
                writer_seq,
                item_key: sealed.item_key.to_vec(),
                op: op.clone(),
                entry: Some(sealed.envelope.clone()),
                feed_seq: None,
            })
            .await
            .context("account-state put: relay plane")?;

        if !self.bound() {
            // The peer leg: no write kind exists on the peer channel, and our
            // frontier slot (the published high-water) must not advance — the
            // nest leg's publish_pending is what sends this row onward.
            return Ok(());
        }

        // Part (2): the rows this one covers — the caller's, the carry's,
        // and every other writer's row at this item key that the outgoing
        // entry covers. Each is named once, at most the wire's bound of them.
        let carried = self
            .carry_names
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&writer_seq)
            .cloned()
            .unwrap_or_default();
        let mut named: Vec<RelayRow> = replaces.to_vec();
        named.extend(carried.iter().cloned());
        named.extend(self.rows_covered_by(plaintext, &sealed.item_key).await?);
        let mut seen = std::collections::BTreeSet::new();
        named.retain(|row| seen.insert((row.writer, row.writer_seq, row.item_key.clone())));
        named.truncate(MAX_REPLACED_ROWS_PER_PUT);
        let replaces_wire = self.replaces_on_wire(&named)?;

        let sent: std::result::Result<AccountStatePutReply, R::Error> = self
            .rpc
            .request(
                KIND_STATE_PUT,
                AccountStatePutRequest {
                    scope: self.scope.clone(),
                    writer_id: writer.to_hex(),
                    writer_seq: writer_seq as i64,
                    item_key: sealed.item_key.to_vec().into(),
                    op,
                    entry: sealed.envelope.into(),
                    // Multi-master: concurrent writes are the merge seam's
                    // input, not a conflict. A nest-arbitrated kind would set
                    // this; none is registered yet (§ Merge-policy seam,
                    // `NestCas`).
                    cas_base: None,
                    replaces: replaces_wire,
                    extra: Default::default(),
                },
            )
            .await;
        let reply = match sent {
            Ok(reply) => reply,
            Err(e) => {
                let text = e.to_string();
                if text.contains("scope_full") {
                    // The count cap (charter § The generation machinery →
                    // *Fleet-scope reclamation*, clause (5)): a full scope is
                    // recoverable with no user act — a pass's retires need no
                    // headroom — so the refusal names them rather than
                    // reading as a dead end, and is typed so the walk's carry
                    // can count it and go on ([`WalkReport::carry_scope_full`]).
                    return Err(ScopeFull {
                        scope: self.scope.clone(),
                        refusal: text,
                    }
                    .into());
                }
                if (self.refused_for_good)(&e) {
                    // The nest's FINAL word on this coordinate (`stale_writer_seq`
                    // — a replay of a row it holds, or a coordinate another life
                    // spent): it will never hold THIS row here, so the relay row
                    // recorded above must not be served to a peer as this
                    // replica's word (refinement 11 → *a refused row's relay
                    // residue*). The local row stays — it is durable and its value
                    // lives in the entry — and the walk's own verdict settles the
                    // rest: a self-echo (the nest holds the row: a replay)
                    // re-records the relay row from the served envelope and
                    // advances the slot; a burnt verdict rotates the writer, and
                    // the heal re-authors the value under the successor. A
                    // transport fault takes the other branch and keeps the row,
                    // exactly as the outage law wants.
                    let retired = self
                        .store
                        .retire_relay_rows_at(&self.scope, &writer, writer_seq)
                        .await
                        .context("account-state put: retiring the refused row's relay row")?;
                    tracing::warn!(
                        writer = %writer.to_hex(),
                        seq = writer_seq,
                        scope = %self.scope,
                        retired,
                        "account-state put: the nest refused this coordinate for good — the relay \
                         row recorded before the send is retired so no peer is served a row the \
                         nest will never hold; the local row stays and the walk's own verdict \
                         (self-echo for a replay, burnt for a reused coordinate) settles what \
                         follows (refinement 11)"
                    );
                    return Err(CoordinateRefused { refusal: text }.into());
                }
                bail!("{KIND_STATE_PUT}: {e}");
            }
        };

        self.store
            .advance_frontier(&self.scope, &writer, writer_seq)
            .await
            .context("account-state put: accounting our own published row")?;
        self.note_acked(
            &writer,
            &sealed.item_key,
            writer_seq,
            self.feed_coordinate_of(reply.seq),
        );
        self.forget_replaced(&named).await?;
        if !carried.is_empty() {
            self.carry_names
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&writer_seq);
        }
        // The nest's serve coordinate for our row (`RelayRow::feed_seq`) — what
        // the reclamation pass compares against the gate's watermark before
        // it asks to retire the row later. Best-effort like the relay row
        // itself: a reply that names no coordinate leaves it unknown, and the
        // walk's own echo fills it in.
        if let Ok(feed_seq) = u64::try_from(reply.seq) {
            self.store
                .stamp_relay_feed_seq(&self.scope, &writer, &sealed.item_key, writer_seq, feed_seq)
                .await
                .context("account-state put: stamping the relay row's feed coordinate")?;
        }
        Ok(())
    }

    /// Send one of this replica's relay rows to the bound nest **verbatim**
    /// — its stored envelope, original `writer_id` and `writer_seq` — the
    /// publish diff's put (`account-sync-plane.md` § The bind leg, ruling 1;
    /// the caller, `crate::publish_diff`, decides which rows may go). A row's
    /// coordinates are its identity and only its writer could re-seal it, so
    /// the bytes go as they are.
    ///
    /// A final refusal (`stale_writer_seq` — the nest holds or has retired
    /// something at or above this coordinate) is final for this copy, ruling
    /// 1(c): the relay row is retired, exactly as [`Self::publish`] retires
    /// its own refused row, and the next walk re-records whatever the nest
    /// holds live for the pair. `scope_full` is a verdict too; a transport
    /// fault is the `Err`.
    ///
    /// `replaces` names the rows this one covers ([`Self::replaces_on_wire`]);
    /// a caller that names none passes an empty slice.
    pub async fn push_verbatim(&self, row: &RelayRow, replaces: &[RelayRow]) -> Result<Pushed> {
        if !self.publish_to_feed {
            bail!("the peer leg sends nothing: a verbatim push is the nest leg's");
        }
        let replaces_wire = self.replaces_on_wire(replaces)?;
        let item_key = <[u8; 32]>::try_from(row.item_key.as_slice())
            .context("verbatim push: the relay row's item key is not 32 bytes")?;
        let entry = row
            .entry
            .clone()
            .context("verbatim push: the relay row carries no entry")?;
        let sent: std::result::Result<AccountStatePutReply, R::Error> = self
            .rpc
            .request(
                KIND_STATE_PUT,
                AccountStatePutRequest {
                    scope: self.scope.clone(),
                    writer_id: row.writer.to_hex(),
                    writer_seq: i64::try_from(row.writer_seq)
                        .context("verbatim push: writer_seq exceeds i64")?,
                    item_key: item_key.to_vec().into(),
                    op: row.op.clone(),
                    entry: entry.into(),
                    cas_base: None,
                    replaces: replaces_wire,
                    extra: Default::default(),
                },
            )
            .await;
        match sent {
            Ok(reply) => {
                self.note_acked(
                    &row.writer,
                    &item_key,
                    row.writer_seq,
                    self.feed_coordinate_of(reply.seq),
                );
                self.forget_replaced(replaces).await?;
                if let Some(feed_seq) = self.feed_coordinate(reply.seq) {
                    self.store
                        .stamp_relay_feed_seq(
                            &self.scope,
                            &row.writer,
                            &item_key,
                            row.writer_seq,
                            feed_seq,
                        )
                        .await
                        .context("verbatim push: stamping the relay row's feed coordinate")?;
                }
                Ok(Pushed::Acked)
            }
            Err(e) if (self.refused_for_good)(&e) => {
                self.store
                    .retire_relay_rows_at(&self.scope, &row.writer, row.writer_seq)
                    .await
                    .context("verbatim push: retiring the refused copy")?;
                Ok(Pushed::RefusedForGood)
            }
            Err(e) if e.to_string().contains("scope_full") => Ok(Pushed::ScopeFull),
            Err(e) => bail!("{KIND_STATE_PUT} (verbatim): {e}"),
        }
    }

    /// **The rows an own put of `plaintext` covers**
    /// (`delegable-scope-reclamation.md` § Delegable-scope reclamation, part
    /// (2)): every relay row at `item_key` under another writer that this
    /// plane opens to an entry `plaintext` covers
    /// ([`crate::delegable_reclaim::entry_covers`]). The outgoing row is the
    /// item's latest once it lands, so an equal row is below it as surely as a
    /// row whose entry lost the merge. Never a row this plane cannot open;
    /// nothing on any scope but the delegable one.
    async fn rows_covered_by(
        &self,
        plaintext: &EntryPlaintext,
        item_key: &[u8; 32],
    ) -> Result<Vec<RelayRow>> {
        if self.scope != ACCOUNT_STATE_SCOPE {
            return Ok(Vec::new());
        }
        let Some(policy) = self.kinds().merge_policy(&plaintext.kind) else {
            return Ok(Vec::new());
        };
        let me = self.store.writer();
        let mut named = Vec::new();
        for row in self.relay_rows_at(item_key).await? {
            if row.writer == me {
                continue;
            }
            if let Some(opened) = self.open_relay_row(&row).await?
                && crate::delegable_reclaim::entry_covers(policy, plaintext, &opened)
            {
                named.push(row);
            }
        }
        Ok(named)
    }

    /// The merge policy this plane applies to `kind` — the compiled table,
    /// then the account's admitted-kinds overlay.
    pub fn merge_policy(&self, kind: &str) -> Option<fauna_protocol::merge_policy::MergePolicy> {
        self.kinds().merge_policy(kind)
    }

    /// The `replaces` list a put sends for `rows` — the rows the put covers,
    /// each named by its cleartext coordinates (`delegable-scope-reclamation.md`
    /// § Delegable-scope reclamation, part (2)). Which rows to name is the
    /// caller's to decide; this checks only what the nest would refuse
    /// anyway: a row of another scope, a list on the fleet scope (whose
    /// retires carry orderings the gate enforces), a list over
    /// [`MAX_REPLACED_ROWS_PER_PUT`].
    fn replaces_on_wire(&self, rows: &[RelayRow]) -> Result<Vec<ReplacedRow>> {
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        if self.scope == ACCOUNT_STATE_FLEET_SCOPE {
            bail!("a put on the fleet scope names no row it replaces");
        }
        if rows.len() > MAX_REPLACED_ROWS_PER_PUT {
            bail!(
                "a put names at most {MAX_REPLACED_ROWS_PER_PUT} rows it replaces, not {}",
                rows.len()
            );
        }
        rows.iter()
            .map(|row| {
                if row.scope != self.scope {
                    bail!("a put names rows of its own scope only");
                }
                Ok(ReplacedRow {
                    item_key: row.item_key.clone().into(),
                    writer_id: row.writer.to_hex(),
                    writer_seq: i64::try_from(row.writer_seq)
                        .context("replaced row: writer_seq exceeds i64")?,
                    extra: Default::default(),
                })
            })
            .collect()
    }

    /// After a put that named `replaces` landed: the nest read the list, and
    /// every row named is either superseded by this put or was not live
    /// there — so the relay copy of each goes (part (2)). Forgotten by exact
    /// `(writer, writer_seq)`, so a newer row of the same writer this replica
    /// holds and did not name stays.
    async fn forget_replaced(&self, replaces: &[RelayRow]) -> Result<()> {
        for row in replaces {
            self.store
                .retire_relay_rows_at(&self.scope, &row.writer, row.writer_seq)
                .await
                .context("account-state put: forgetting a replaced row's relay copy")?;
        }
        Ok(())
    }

    // ── Reclamation (charter § The generation machinery → *Fleet-scope
    // reclamation*) ────────────────────────────────────────────────────────

    /// Ask the nest to retire one live row of this scope by its cleartext
    /// coordinates — `fauna.account.state.retire`, clause (1): the row is
    /// marked superseded and NOTHING is inserted, so this needs no cap
    /// headroom. The nest keeps the row's bytes and coordinate.
    ///
    /// What comes back is a verdict, never an error, for every answer the
    /// ruling names: the nest's retention gate (`not_yet_stable`), its
    /// generation belt (`generation_in_use`), and the peer leg, which serves no
    /// write kind (`Unsupported` — the row simply stays, today's growth). Any
    /// other refusal, and a transport fault, is the `Err`.
    ///
    /// With `delete_escrow_wraps` beside `no_rows_sealed_under`, the retire
    /// that lands also takes the nest's escrow wraps of that generation with
    /// it (clause (3e)'s sweep) — the caller's vouch that the generation is
    /// shredded, the nest's belt the check.
    ///
    /// Local state is untouched on purpose: this replica's merged rows and its
    /// relay plane keep the row (every reader makes its own redundancy
    /// decision; a retired row is redundant by construction).
    pub async fn retire(
        &self,
        item_key: &[u8; 32],
        writer: &WriterId,
        writer_seq: u64,
        no_rows_sealed_under: Option<&[u8; 32]>,
        delete_escrow_wraps: bool,
    ) -> Result<RetireOutcome>
    where
        R::Error: fauna_protocol::RpcErrorClass,
    {
        self.retire_row(
            item_key,
            writer,
            writer_seq,
            no_rows_sealed_under,
            delete_escrow_wraps,
            true,
        )
        .await
    }

    /// [`Self::retire`] with no belt, and **never entered in the store's
    /// retire record** — the secondary leg's removed-device arm at the bound
    /// nest (`account-sync-plane.md` § The bind leg, ruling 7(c);
    /// `crate::linked_leg::retire_carried_evidence`). That arm asks each
    /// linked nest itself, in the same run and only where that nest's own
    /// replicas were all reached; a recorded entry would be re-issued by cell
    /// at the next run (`crate::linked_leg::mirrored_retires`) and walk round
    /// that check.
    pub async fn retire_unrecorded(
        &self,
        item_key: &[u8; 32],
        writer: &WriterId,
        writer_seq: u64,
    ) -> Result<RetireOutcome>
    where
        R::Error: fauna_protocol::RpcErrorClass,
    {
        self.retire_row(item_key, writer, writer_seq, None, false, false)
            .await
    }

    async fn retire_row(
        &self,
        item_key: &[u8; 32],
        writer: &WriterId,
        writer_seq: u64,
        no_rows_sealed_under: Option<&[u8; 32]>,
        delete_escrow_wraps: bool,
        record: bool,
    ) -> Result<RetireOutcome>
    where
        R::Error: fauna_protocol::RpcErrorClass,
    {
        use fauna_protocol::account_state::{
            AccountStateRetireReply, AccountStateRetireRequest, KIND_STATE_RETIRE,
        };
        if !self.publish_to_feed {
            // The peer channel serves no write kind; the nest leg reclaims.
            return Ok(RetireOutcome::Unsupported);
        }
        let reply: Result<AccountStateRetireReply, R::Error> = self
            .rpc
            .request(
                KIND_STATE_RETIRE,
                AccountStateRetireRequest {
                    scope: self.scope.clone(),
                    item_key: item_key.to_vec().into(),
                    writer_id: writer.to_hex(),
                    writer_seq: writer_seq as i64,
                    no_rows_sealed_under: no_rows_sealed_under.map(|g| g.to_vec().into()),
                    delete_escrow_wraps,
                    extra: Default::default(),
                },
            )
            .await;
        let outcome = match reply {
            Ok(reply) if reply.retired => RetireOutcome::Retired,
            Ok(_) => RetireOutcome::Gone,
            Err(e) => match fauna_protocol::RpcErrorClass::as_rpc_error(&e) {
                Some(rpc) if rpc.code.ends_with("not_yet_stable") => RetireOutcome::NotYetStable,
                Some(rpc) if rpc.code.ends_with("generation_in_use") => {
                    RetireOutcome::GenerationInUse
                }
                _ => return Err(anyhow::anyhow!("{KIND_STATE_RETIRE}: {e}")),
            },
        };
        // Retires follow the rows (`account-sync-plane.md` § The bind leg,
        // ruling 4): what the bound nest was asked, each linked nest whose
        // listing still shows the row is asked too. Deferred answers are
        // recorded as well — every nest's gate and belt answer for its own
        // rows. The record rests in the store (ruling 5), so the process that
        // runs the secondary leg reads what whichever process pumps sent —
        // one path, also where both are this runtime. Best-effort: the retire
        // itself landed, and a lost entry delays it at a secondary until a
        // later pass re-makes the entry, or leaves a redundant row there
        // (ruling 6). An answer of `Gone` is recorded too: it is how a row
        // only a secondary still holds gets its entry.
        if record && self.bound() {
            let recorded = self
                .store
                .record_issued_retire(&IssuedRetire {
                    scope: self.scope.clone(),
                    item_key: *item_key,
                    writer: *writer,
                    writer_seq,
                    no_rows_sealed_under: no_rows_sealed_under.copied(),
                    delete_escrow_wraps,
                    settled: matches!(outcome, RetireOutcome::Retired | RetireOutcome::Gone),
                })
                .await;
            if let Err(e) = recorded {
                tracing::warn!(
                    scope = %self.scope,
                    "a retire could not be recorded for the linked nests: {e:#}"
                );
            }
        }
        Ok(outcome)
    }

    /// Does the nest serve any live row of this scope sealed under
    /// `generation` (clause (3e)'s datalessness question, asked of the nest —
    /// never inferred from this replica's relay plane, which is never told
    /// about other writers' retirements)? One page suffices: the answer is
    /// "any", not "how many".
    pub async fn any_row_sealed_under(&self, generation: &[u8; 32]) -> Result<bool> {
        let reply: SyncChangesListReply = self
            .rpc
            .request(
                "fauna.sync.changes.list",
                SyncChangesListRequest {
                    since: NEST_SLOT_UNUSED,
                    item_class: Some(ItemClass::StateEntry.as_wire().to_string()),
                    scope: Some(self.scope.clone()),
                    frontier: Some(BTreeMap::new()),
                    sealed_under: Some(generation.to_vec().into()),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| anyhow::anyhow!("fauna.sync.changes.list (sealed_under): {e}"))?;
        Ok(!reply.changes.is_empty())
    }

    /// Open one of this replica's relay rows (a verbatim wire row it walked
    /// or authored) to its kind + key + value — the reclamation pass's way of
    /// classifying another writer's rows by what they are. `None` for a row
    /// this build cannot open (a kind it predates, a generation it cannot
    /// key, a tampered envelope) — such a row is left alone. An attested
    /// predecessor's row is one of those here, whatever keys the plane was
    /// handed: this open tries the plane's own keys only, so the publish
    /// diff never vouches a row sealed under a retired schedule
    /// (`succession-aftermath.md` § Re-key scope → *Which walks carry*). The
    /// retired machinery keys are tried by
    /// [`Self::open_predecessor_machinery_row`], whose answer only the
    /// reclamation pass's predecessor arm reads, to retire.
    pub async fn open_relay_row(&self, row: &RelayRow) -> Result<Option<EntryPlaintext>> {
        let Some(envelope) = row.entry.as_deref() else {
            return Ok(None);
        };
        let Ok(item_key) = <[u8; 32]>::try_from(row.item_key.as_slice()) else {
            return Ok(None);
        };
        let coords = EntryCoordinates {
            writer_id: row.writer.0,
            writer_seq: row.writer_seq,
            scope: &self.scope,
        };
        let mut memo = BTreeMap::new();
        self.trial_open(
            &coords,
            &item_key,
            envelope,
            &mut memo,
            &mut Default::default(),
        )
        .await
    }

    /// The blinded item key of a **gen-0** (form v1) row of `kind` at `key` —
    /// what the nest knows the row by. Only meaningful for `Gen0` kinds: a
    /// tip-sealed row's item key derives from the per-generation schedule
    /// (the relay row carries it; see [`Self::open_relay_row`]).
    pub fn gen0_item_key(&self, kind: &str, key: &str) -> Option<[u8; 32]> {
        if self.kinds().sealing_epoch(kind) != Some(SealingEpoch::Gen0) {
            return None;
        }
        self.kinds()
            .kind_keys(self.schedule, kind)
            .map(|keys| keys.item_key(key.as_bytes()))
    }

    /// The live relay rows of this scope's state entries at one blinded item
    /// key, one per writer — the reclamation pass's "who still holds a live
    /// row for this item, at which coordinate", answered by index.
    pub async fn relay_rows_at(&self, item_key: &[u8; 32]) -> Result<Vec<RelayRow>> {
        self.store
            .relay_rows_at(&self.scope, ItemClass::StateEntry.as_wire(), item_key)
            .await
    }

    /// Drop one of this replica's relay rows locally — after its writer
    /// retired it from the feed (the row is nothing a peer should be served,
    /// and nothing a later pass should keep naming).
    pub async fn relay_forget(&self, writer: &WriterId, item_key: &[u8]) -> Result<()> {
        self.store.relay_forget(&self.scope, writer, item_key).await
    }

    /// The logical `(kind, key)` this replica's OWN journal holds at
    /// `writer_seq` in this scope — the link from one of our relay rows
    /// (which carries only the blinded item key) back to what it is, with no
    /// AEAD: our journal and our relay plane share the writer seq.
    pub async fn own_journal_item(&self, writer_seq: u64) -> Result<Option<ItemId>> {
        Ok(self
            .store
            .scope_rows(
                &self.scope,
                &self.store.writer(),
                writer_seq.saturating_sub(1),
                1,
            )
            .await?
            .into_iter()
            .find(|row| row.seq == writer_seq)
            .and_then(|row| match row.item {
                ItemRef::StateKey { kind, key, .. } => Some(ItemId { kind, key }),
                _ => None,
            }))
    }

    /// One writer's live relay rows of this scope's state entries — the
    /// reclamation pass's view of a removed writer's rows, and a signing-out
    /// device's view of its own.
    pub async fn relay_rows_of_writer(&self, writer: &WriterId) -> Result<Vec<RelayRow>> {
        self.store
            .relay_rows_of_writer(&self.scope, ItemClass::StateEntry.as_wire(), writer)
            .await
    }

    /// The **R14 admission** at the writer door — the gate's real shape
    /// (charter § The generation machinery → *The sealing-epoch axis, and the
    /// gate's real shape*): resolve the current admissible, escrow-acked
    /// generation tip, and refuse a `GenerationTip` origination if none
    /// exists. That one check *is* escrow-before-first-seal (a
    /// minted-but-unacked generation resolves to the prior tip; no prior tip
    /// → refuse), *is* the R14 gate while no generation exists, and lifts
    /// automatically — per replica, honestly — the moment a mint's escrow
    /// receipt lands in merged state. `Gen0` kinds (every delegable kind, and
    /// the machinery kinds themselves) are admitted without resolving: their
    /// branch never gains a generation axis, which is what dissolves the
    /// machinery's apparent self-gating.
    ///
    /// Like its boolean predecessor this guards what the replica
    /// **originates**, never what [`Self::publish`] relays for other writers
    /// (see the module's client-recoverability reasoning on the walk's
    /// republish path), and it runs LAST of the preflight checks so
    /// policy-specific complaints keep their own voices.
    ///
    /// The door also holds the **A5 partition**: every class-2 kind has
    /// exactly one home scope (delegable → `state`, fleet-only →
    /// `state-fleet`, `fauna_protocol::merge_policy::home_scope_for_kind`),
    /// and an origination into any other scope would put rows where the
    /// partition promises they never appear — a delegable subscriber must not
    /// see fleet churn. Door-only, like the epoch check: a hostile row in the
    /// wrong scope is the walk's skip business, never a publish abort.
    async fn admit_origination(&self, kind: &str) -> Result<()> {
        if let Some(home) = home_scope_for_kind(kind)
            && home != self.scope
        {
            bail!(
                "kind {kind:?} seals into scope {home:?} and this plane serves {:?} — the A5 \
                 partition routes every class-2 kind to exactly one home scope \
                 (account-data-taxonomy.md § The generation machinery, the partition bullet)",
                self.scope
            );
        }
        match self.kinds().sealing_epoch(kind) {
            // Unregistered kinds were refused by the merge-policy preflight
            // already; epoch registration is total over registered kinds
            // (pinned in `fauna_protocol::merge_policy`'s tests).
            None | Some(SealingEpoch::Gen0) => Ok(()),
            Some(SealingEpoch::GenerationTip) => self.resolve_or_mint(kind).await.map(|_| ()),
        }
    }

    /// Whether an origination of `kind` through this plane's writer door
    /// would run the first-need mint — the one step of a local write that is
    /// network (an escrow deposit, then the mint's own publish; or, after a
    /// pin move, the re-escrow's deposits in its place) and that
    /// takes the plane's single-flight mint lock. `true` only for a
    /// `GenerationTip` kind at its home scope, on a plane that publishes,
    /// when no admissible tip resolves for this device now; everything else
    /// is admitted, or refused, by store reads alone.
    ///
    /// The account runtime asks it before serving a tip-sealed door put
    /// inside a pass (`Cmd::is_local`, the door-put verdict): the pass is not
    /// polled while a command is served, so the answer still holds when the
    /// write that follows it runs, with no step of the pass between the two
    /// (native store calls are synchronous besides; on web a transaction the
    /// pass had already placed may still commit there).
    pub async fn origination_mints(&self, kind: &str) -> Result<bool> {
        if home_scope_for_kind(kind).is_some_and(|home| home != self.scope)
            || !self.bound()
            || !matches!(sealing_epoch(kind), Some(SealingEpoch::GenerationTip))
        {
            return Ok(false);
        }
        Ok(
            generation_tip::resolve_tip(self.store, self.trust, self.writer_key, self.custody)
                .await?
                .tip
                .is_none(),
        )
    }

    /// The door's tip question, with the mint protocol's **trigger (a)**
    /// attached: "a `GenerationTip` origination finds no admissible acked tip
    /// and the engine mints instead of refusing forever (provided an escrow
    /// target exists and a holder is reachable; otherwise the refusal stands
    /// and says why)" — charter § The generation machinery → *The mint
    /// protocol*.
    ///
    /// The trigger lives **here and not in a background pass** because the
    /// write that refuses is the write that must end up succeeding: this door
    /// deliberately keeps no durable local row for a refused `GenerationTip`
    /// origination, so a pass that minted a second later would leave the user's
    /// write already failed with nothing to retry.
    ///
    /// It fires whenever **no candidate resolves for this observer** — the
    /// candidate-aware first-need ratified with the ST-007 fix (charter § The
    /// mint protocol). The initial landing's narrower guard — refuse whenever
    /// *any* mint row exists — was itself the wedge ST-007 weaponized: an
    /// attacker's excluding mint, or an honest crash-orphaned unacked one,
    /// blocked re-minting forever. The heal-mint keeps the DAG connected by
    /// naming the current leaves as its parents (capped, byte-order max
    /// preferred), so it supersedes what it heals across rather than forking
    /// a parentless second root; on a fresh account the leaf set is empty and
    /// this is the ordinary "empty list is the first generation" mint.
    async fn resolve_or_mint(&self, kind: &str) -> Result<AdmissibleTip> {
        let resolution =
            generation_tip::resolve_tip(self.store, self.trust, self.writer_key, self.custody)
                .await?;
        if let Some(tip) = resolution.tip {
            return Ok(tip);
        }
        // The peer leg never mints: its `rpc` is a peer-channel requester
        // serving only the sync-transfer kinds, so there is no escrow door
        // behind it — and a generation minted without a deposit is exactly
        // what escrow-before-first-seal forbids.
        if !self.bound() {
            return Err(anyhow::anyhow!(self.no_tip_refusal(kind, &resolution)));
        }
        let _single_flight = self.mint_lock.lock().await;
        // Re-resolve under the lock: a concurrent origination may have minted
        // while this one waited, and minting again would fork the DAG on the
        // account's very first write.
        let resolution =
            generation_tip::resolve_tip(self.store, self.trust, self.writer_key, self.custody)
                .await?;
        if let Some(tip) = resolution.tip {
            return Ok(tip);
        }
        // A holder change re-receipts and never mints (`account-data-taxonomy.md`
        // § The generation machinery, (2)): a tip receipted for this identity
        // by a holder the pin has moved off is owed a deposit, not a mint —
        // made here when this door meets it before the pass's re-escrow did.
        // Boxed: its receipt rows go back through this door (`Gen0`, so one
        // level deep), the cycle `mint_first_need` boxes for the same reason.
        match Box::pin(crate::generation_reescrow::reescrow_owed_at_the_door(
            self.store,
            self,
            self.trust,
            self.writer_key,
        ))
        .await
        {
            Ok(false) => {}
            Ok(true) => {
                let resolution = generation_tip::resolve_tip(
                    self.store,
                    self.trust,
                    self.writer_key,
                    self.custody,
                )
                .await?;
                if let Some(tip) = resolution.tip {
                    return Ok(tip);
                }
            }
            Err(why) => return Err(why.context(self.no_tip_refusal(kind, &resolution))),
        }
        let parents = heal_parents(&resolution.leaf_ids);
        if let Err(why) = self.mint_first_need(parents).await {
            // "The refusal stands and says why": the standing no-tip refusal
            // is what the caller still gets, with the half that actually
            // failed — no escrow target, no trusted holder, no reachable door
            // — as its cause.
            return Err(why.context(self.no_tip_refusal(kind, &resolution)));
        }
        let resolution =
            generation_tip::resolve_tip(self.store, self.trust, self.writer_key, self.custody)
                .await?;
        resolution.tip.ok_or_else(|| {
            anyhow::anyhow!(
                "the first-need mint for kind {kind:?} completed and published its rows, yet no \
                 tip resolves from them — the mint sequence and the resolver disagree, which is \
                 a build defect rather than a state this account can reach ({} row(s) flagged \
                 invalid during resolution)",
                resolution.invalid.len()
            )
        })
    }

    /// Resolve the tip for a `GenerationTip` **seal**, or refuse with the
    /// precise reason ("fleet-only sealing stays refused **and says why**").
    ///
    /// Deliberately mint-free, unlike [`Self::resolve_or_mint`]: this runs at
    /// seal time, which [`Self::publish_pending`] re-enters for every
    /// local-only row on every pass. A mint here would turn a stuck publish
    /// into a mint storm, and the origination that created the row already
    /// passed the door that owns the trigger.
    async fn resolved_tip_or_refuse(&self, kind: &str) -> Result<AdmissibleTip> {
        let resolution =
            generation_tip::resolve_tip(self.store, self.trust, self.writer_key, self.custody)
                .await?;
        let refusal = self.no_tip_refusal(kind, &resolution);
        resolution.tip.ok_or_else(|| anyhow::anyhow!(refusal))
    }

    /// The standing R14 refusal text, shared by the door and the seal so the
    /// two paths cannot drift into two different explanations of one gate.
    fn no_tip_refusal(&self, kind: &str, resolution: &TipResolution) -> String {
        format!(
            "kind {kind:?} is registered `GenerationTip`, and no candidate generation tip \
             resolves for this device from merged plane state — sealing it stays refused \
             until a mint's escrow receipt lands (this one check is escrow-before-first-seal \
             and the R14 gate in one; {} row(s) flagged invalid and {} unreachable from this \
             device during resolution — an unreachable mint is healed by a top-up from any \
             key-holding device; account-data-taxonomy.md § The generation machinery)",
            resolution.invalid.len(),
            resolution.unkeyable.len()
        )
    }

    /// Run the mint sequence for a first-need mint — `parents` from the
    /// resolver's leaf set ([`heal_parents`]) — and write its rows through
    /// this same door: `Gen0` machinery, admitted without resolving anything.
    ///
    /// Deposit-first is the sequence's own crash-safety shape
    /// (`generation_mint`): until the holder has answered nothing is staged
    /// locally, so the door's "a refused `GenerationTip` write leaves no
    /// durable local row" contract survives a failed mint unchanged.
    ///
    /// **The deposit is the one step that must be online; the rows are
    /// local-first** (`account-data-taxonomy.md` § The generation machinery →
    /// *The mint protocol*, the mint sequence). They are written with
    /// [`Self::put_local`] and then sent in one ordered publish, and a send
    /// that fails does not undo the mint: the holder's receipt is in hand and
    /// durable, the tip resolves on this replica, and [`Self::publish_pending`]
    /// sends the rows — ahead of anything sealed under them, by journal order
    /// — on the next pass. Sending row by row instead aborted the sequence
    /// between the mint row and the receipt row whenever the state put failed
    /// after the deposit landed: the write that tripped the mint was refused,
    /// an unacked mint row stayed behind, and every retry spent another
    /// deposit on another generation.
    async fn mint_first_need(&self, parents: Vec<[u8; 32]>) -> Result<()> {
        let minted = crate::generation_mint::mint_generation(
            self.store,
            self.rpc,
            &crate::generation_mint::MintContext {
                root: &self.trust.root,
                // "Any enrolled device may mint" — this one, which the mint
                // sequence re-checks against the merged fleet view.
                minter_key: self.writer_key,
                trusted_holders: &self.trust.trusted_holders.get(),
            },
            parents,
            fauna_core::data::Timestamp::now_millis_or_zero() as i64,
        )
        .await?;
        // The minter is the first holder: the fresh key rides the retained
        // bundle from birth (W5.4a carriage) — for a later shredded mint the
        // bundle is the only read path left.
        if let Some(custody) = self.custody {
            custody.record_generation_key(&minted.generation_id, &minted.gen_key);
        }
        // Log order is the contract (charter § The mint protocol → *The
        // bounded mint*): the mint row, then one ordinary top-up per SPILLED
        // member (the bounded mint's members without an inline wrap — the
        // minter is the first healer, through the same `put_heal` the pass
        // uses), and the receipt row LAST. The receipt is what the resolver
        // checks for escrow-acked, so a crash anywhere before it leaves an
        // unacked mint — ignored by resolution, superseded by the next one —
        // rather than an ack for a mint some member cannot yet key; and since
        // one writer's rows are served in log order, a reader that holds the
        // receipt already holds every wrap that precedes it.
        //
        // Each write is boxed because it closes a cycle in the future's own
        // type — `put_local` → `admit_origination` → here → `put_local` —
        // which rustc must be able to size. The recursion is one level deep
        // by construction: every row written here is a `Gen0` machinery kind,
        // so the inner write is admitted without ever resolving a tip.
        let mint_item = ItemId {
            kind: minted.mint_entry.kind,
            key: minted.mint_entry.key,
        };
        Box::pin(self.put_local(
            &mint_item,
            minted.mint_entry.value,
            minted.mint_entry.merge_meta,
        ))
        .await
        .context("writing the first-need mint's row")?;
        for target in &minted.spilled {
            Box::pin(crate::generation_topup::put_heal_local(
                self,
                &minted.generation_id,
                target,
                &minted.gen_key,
                self.writer_key,
            ))
            .await
            .context("writing the first-need mint's spill top-ups")?;
        }
        let receipt_item = ItemId {
            kind: minted.receipt_entry.kind,
            key: minted.receipt_entry.key,
        };
        Box::pin(self.put_local(
            &receipt_item,
            minted.receipt_entry.value,
            minted.receipt_entry.merge_meta,
        ))
        .await
        .context("writing the first-need mint's receipt row")?;
        // The rows are durable and the generation is acked here. One ordered
        // send now, so an online mint reaches the fleet at once; a failure is
        // the next pass's `publish_pending` to retry, never the mint's.
        if let Err(e) = self.publish_pending().await {
            tracing::warn!(
                scope = %self.scope,
                "the first-need mint's rows are written and not yet sent ({e:#}) — the \
                 generation is acked on this replica, and the next pass's publish step \
                 sends them ahead of every row sealed under it"
            );
        }
        Ok(())
    }

    /// Seal one plaintext in its kind's registered form: gen-0 **v1** for
    /// `Gen0` kinds (unchanged), generation-sealed **v2** under the resolved
    /// tip for `GenerationTip` kinds — the writer opens its own inline or
    /// top-up wrap via the device KEM secret derived from its writer key
    /// (`generation_tip::key_for_tip`), and the per-generation schedule
    /// derives from the recovered key
    /// ([`FleetOnlySchedule::derive_for_generation`]).
    ///
    /// Resolution happens **at seal time**: a re-publish
    /// ([`Self::publish_pending`]) after the tip moved seals under the
    /// *current* tip — "GenerationTip originations seal form v2 under the
    /// resolved tip's key" is true of whichever publish attempt succeeds, and
    /// a seal the replica cannot perform (no tip, no wrap yet) returns the
    /// precise error while the local row stays durable for the retry.
    async fn seal_for_kind(
        &self,
        plaintext: &EntryPlaintext,
        writer_seq: u64,
    ) -> Result<fauna_core::account_entry_crypto::SealedEntry> {
        let writer = self.store.writer();
        let coords = EntryCoordinates {
            writer_id: writer.0,
            writer_seq,
            scope: &self.scope,
        };
        match sealing_epoch(&plaintext.kind) {
            Some(SealingEpoch::GenerationTip) => {
                let tip = self.resolved_tip_or_refuse(&plaintext.kind).await?;
                let gen_key =
                    generation_tip::key_for_tip(self.store, &tip, self.writer_key, self.custody)
                        .await
                        .with_context(|| {
                            format!(
                                "sealing a {:?} entry under the resolved tip",
                                plaintext.kind
                            )
                        })?;
                let keys =
                    FleetOnlySchedule::derive_for_generation(&gen_key).for_kind(&plaintext.kind);
                seal_entry_v2(
                    &keys,
                    &coords,
                    &tip.generation_id,
                    plaintext,
                    self.writer_key,
                )
                .context("account-state put: seal (form v2)")
            }
            Some(SealingEpoch::Gen0) => {
                let keys = self
                    .kinds()
                    .kind_keys(self.schedule, &plaintext.kind)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "kind {:?} has no registered audience rung — its key branch is \
                         undecided, so there is no correct way to seal it",
                            plaintext.kind
                        )
                    })?;
                seal_entry(&keys, &coords, plaintext, self.writer_key)
                    .context("account-state put: seal")
            }
            None => bail!(
                "kind {:?} has no registered sealing epoch — its key branch is undecided, so \
                 there is no correct way to seal it",
                plaintext.kind
            ),
        }
    }

    /// Re-publish every local row above our published high-water — the recovery
    /// pass for a write whose network call never landed.
    ///
    /// A row whose item has since been superseded locally is skipped: the later
    /// row carries the current value, and re-sending the older one would be
    /// refused by the nest's non-advancing-`writer_seq` guard anyway. Returns
    /// how many rows were actually sent.
    ///
    /// **On the delegable scope a row refused for room is parked** and the
    /// publish goes on (`account-replica-posture.md` § The store device
    /// principal, refinement 11 → *A row refused for room is parked*): the
    /// row's coordinate is recorded durably ([`AccountStore::park`]), the
    /// slot moves past it as past an acked row, and every publish first
    /// retries the parked rows ([`Self::retry_parked`]). On the fleet scope
    /// a `scope_full` answer stops the publish, as every other failure does
    /// on either scope.
    ///
    /// This is deliberately *not* the offline outbox (charter § The
    /// offline-mutation contract, W4) — it replays this replica's own class-2
    /// rows, nothing else, and it holds no intents.
    pub async fn publish_pending(&self) -> Result<usize> {
        self.publish_pending_below(None).await
    }

    /// Does this plane park a row the nest refuses for room, rather than
    /// stop at it? The delegable scope's items are independent of one
    /// another; the fleet scope's puts are ordered against each other
    /// (refinement 11 → *The fleet scope keeps the stop*).
    fn parks_refused_for_room(&self) -> bool {
        self.bound() && self.scope == ACCOUNT_STATE_SCOPE
    }

    /// The departed list ([`crate::departure::departed_scopes`]) on the
    /// delegable scope, whose member scopes' items send no row; empty on
    /// every other scope.
    pub async fn departed_scopes(&self) -> Result<std::collections::BTreeSet<String>> {
        if self.scope != ACCOUNT_STATE_SCOPE {
            return Ok(Default::default());
        }
        crate::departure::departed_scopes(self.store).await
    }

    /// How many of this writer's rows the plane holds parked
    /// ([`Self::publish_pending`]). Always 0 on a plane that does not park.
    pub async fn parked_count(&self) -> Result<usize> {
        if !self.parks_refused_for_room() {
            return Ok(0);
        }
        Ok(self
            .store
            .parked(&self.scope, &self.store.writer())
            .await?
            .len())
    }

    /// The items this writer holds an **unsent** own row for — journaled above
    /// its published slot, or parked — as `(kind, key)`. The hand-over's
    /// "nothing above its published slot for the item"
    /// (`delegable-scope-reclamation.md` § Delegable-scope reclamation, part
    /// (3)): such an item's next put is already owed.
    pub async fn unsent_own_items(&self) -> Result<std::collections::BTreeSet<(String, String)>> {
        let writer = self.store.writer();
        let mut items = std::collections::BTreeSet::new();
        let mut owed: Vec<u64> = self
            .store
            .parked(&self.scope, &writer)
            .await?
            .into_iter()
            .collect();
        let published = self
            .store
            .frontier(&self.scope)
            .await?
            .into_iter()
            .find(|(w, _)| *w == writer)
            .map(|(_, seq)| seq)
            .unwrap_or(0);
        let mut after = published;
        loop {
            let rows = self
                .store
                .scope_rows(&self.scope, &writer, after, PAGE)
                .await?;
            let Some(last) = rows.last() else { break };
            after = last.seq;
            for row in rows {
                if let ItemRef::StateKey { kind, key, .. } = row.item {
                    items.insert((kind, key));
                }
            }
        }
        owed.sort_unstable();
        for seq in owed {
            if let Some(item) = self.own_journal_item(seq).await? {
                items.insert((item.kind, item.key));
            }
        }
        Ok(items)
    }

    /// [`Self::publish_pending`], stopping short of the row at `below` (and
    /// everything after it) when one is named — [`Self::put_last_word`]'s
    /// drain of the rows ahead of its own.
    async fn publish_pending_below(&self, below: Option<u64>) -> Result<usize> {
        let writer = self.store.writer();
        let parks = self.parks_refused_for_room();
        let mut parked = if parks {
            self.store.parked(&self.scope, &writer).await?
        } else {
            Default::default()
        };
        let departed = self.departed_scopes().await?;
        let mut sent = 0usize;
        if !parked.is_empty() {
            sent += self
                .retry_parked(&writer, below, &mut parked, &departed)
                .await?;
        }
        let published = self
            .store
            .frontier(&self.scope)
            .await?
            .into_iter()
            .find(|(w, _)| *w == writer)
            .map(|(_, seq)| seq)
            .unwrap_or(0);

        let mut after = published;
        'pages: loop {
            let rows = self
                .store
                .scope_rows(&self.scope, &writer, after, PAGE)
                .await?;
            if rows.is_empty() {
                break;
            }
            for row in &rows {
                if below.is_some_and(|below| row.seq >= below) {
                    break 'pages;
                }
                after = row.seq;
                if parked.contains(&row.seq) {
                    // Owed by name: the retry above sent it or met the
                    // refusal again this pass. A list a crash left above the
                    // slot is the one way here.
                    continue;
                }
                let ItemRef::StateKey {
                    kind,
                    key,
                    entry_version,
                } = &row.item
                else {
                    continue; // a class-1 row on our log — not this plane's
                };
                let Some(current) = self.store.state(kind, key).await? else {
                    continue;
                };
                if current.entry_version != *entry_version {
                    // Superseded locally; the later row carries the truth.
                    continue;
                }
                if crate::departure::of_departed_scope(&departed, kind, key) {
                    // A departed scope's item sends no row
                    // (`delegable-scope-reclamation.md` part (6)). While the
                    // row stays above the slot it is owed, and a re-join
                    // sends it; once a later row moves the slot past it, the
                    // re-join's hand-over writes the entry back instead.
                    continue;
                }
                // No row here is over the per-entry cap: every row this
                // plane journals passed `SizedEntry::size` first
                // (`put_own_row` takes nothing else).
                let plaintext = entry_to_plaintext(&current);
                match self.publish(&plaintext, row.seq, &[]).await {
                    Ok(()) => sent += 1,
                    Err(e) if parks && is_scope_full(&e) => {
                        // Parked BEFORE the slot moves past it: a crash
                        // between the two leaves the row parked above the
                        // slot, which the check above skips and the retry
                        // owes; the other order would strand it.
                        self.store.park(&self.scope, &writer, row.seq).await?;
                        parked.insert(row.seq);
                        self.store
                            .advance_frontier(&self.scope, &writer, row.seq)
                            .await
                            .context("account-state publish: the slot past a parked row")?;
                        tracing::info!(
                            scope = %self.scope,
                            seq = row.seq,
                            kind = %kind,
                            "{e:#}"
                        );
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(sent)
    }

    /// The parked rows' retry, ahead of the rows above the slot: a
    /// preference record's row before any other, the rest oldest first, and
    /// no further once one is refused for room again — so a full scope costs
    /// one refused put a pass (refinement 11 → *A row refused for room is
    /// parked*). A row leaves `parked` when the nest accepts it or when the
    /// entry has moved past it; a `stale_writer_seq` answer keeps it for the
    /// walk's own echo to settle and does not stop the rest. Any other
    /// failure is the publish's. Returns how many rows were sent.
    async fn retry_parked(
        &self,
        writer: &WriterId,
        below: Option<u64>,
        parked: &mut std::collections::BTreeSet<u64>,
        departed: &std::collections::BTreeSet<String>,
    ) -> Result<usize> {
        let mut owed: Vec<(bool, u64, StateEntry)> = Vec::new();
        for seq in parked.clone() {
            if below.is_some_and(|below| seq >= below) {
                continue;
            }
            let held = self
                .store
                .scope_rows(&self.scope, writer, seq.saturating_sub(1), 1)
                .await?
                .into_iter()
                .find(|row| row.seq == seq);
            let current = match held.map(|row| row.item) {
                Some(ItemRef::StateKey {
                    kind,
                    key,
                    entry_version,
                }) => self
                    .store
                    .state(&kind, &key)
                    .await?
                    .filter(|current| current.entry_version == entry_version),
                _ => None,
            };
            let Some(current) = current.filter(|current| {
                !crate::departure::of_departed_scope(departed, &current.kind, &current.key)
            }) else {
                // The entry has moved past the row (a later row carries the
                // value, as for any superseded row), the row is gone, or its
                // item's scope departed (`delegable-scope-reclamation.md`
                // part (6)): the entry stays in the store, and a re-join's
                // hand-over writes it back.
                self.store.unpark(&self.scope, writer, seq).await?;
                parked.remove(&seq);
                continue;
            };
            owed.push((
                !crate::preference_put::is_preference_kind(&current.kind),
                seq,
                current,
            ));
        }
        owed.sort_by_key(|(later, seq, _)| (*later, *seq));
        let mut sent = 0usize;
        for (_, seq, current) in owed {
            match self.publish(&entry_to_plaintext(&current), seq, &[]).await {
                Ok(()) => {
                    self.store.unpark(&self.scope, writer, seq).await?;
                    parked.remove(&seq);
                    sent += 1;
                }
                Err(e) if is_scope_full(&e) => break,
                Err(e) if e.downcast_ref::<CoordinateRefused>().is_some() => {
                    tracing::info!(
                        scope = %self.scope,
                        seq,
                        "a parked row's retry was refused for its coordinate — it stays parked \
                         for the walk's own echo to settle ({e:#})"
                    );
                }
                Err(e) => return Err(e),
            }
        }
        Ok(sent)
    }

    // ── Walk ────────────────────────────────────────────────────────────────

    /// Backstop 1 — the accounted catch-up walk from this replica's stored
    /// frontier (charter § Nudges and backstops). The thing a
    /// `fauna.sync.changed` nudge should trigger.
    ///
    /// On the **nest leg** the request is a projection of the stored frontier
    /// under the scope's banked serve-order watermark (charter § Feeds and
    /// cursors → *Compaction is a serve-order watermark*): once the nest has
    /// echoed one, the walk names only the writers with a row seen but never
    /// accounted (`unaccounted_writers`), and the watermark covers everyone
    /// else. Against a nest that has
    /// never echoed, that is the whole stored frontier, as ever.
    pub async fn walk(&self) -> Result<WalkReport> {
        if self.linked {
            // A linked nest keeps no watermark of ours and no frontier is
            // accounted against its log: its catch-up IS the reconcile.
            return self.reconcile().await;
        }
        let mut start = stored_frontier(self.store, &self.scope).await?;
        if !self.publish_to_feed {
            // Peer leg: the stored own-slot is the *published* high-water and
            // deliberately lags (see the self-echo note in `apply`), but we
            // hold every row we authored by definition — seed the paging
            // cursor past them so a peer doesn't re-serve our own log at us
            // on every walk.
            seed_past_own_held(self.store, &self.scope, &mut start).await?;
            // And the whole frontier, never a watermark. A peer serves in
            // `(writer, writer_seq)` order, with no cross-writer order for one
            // to be a coordinate in — the ruling defers this leg — and this
            // is the very store the nest leg banks its watermark in, so a
            // peer's echo taken here would let a counterpart with no serve
            // order at all withhold rows from the nest leg.
            return self.run(start).await;
        }
        let held = self
            .store
            .nest_watermark(&self.scope)
            .await?
            .map(i64::try_from)
            .transpose()
            .context("account-state walk: the stored watermark exceeds i64")?;
        let banked_replica = self.store.nest_watermark_replica(&self.scope).await?;
        let pinned = unaccounted_writers(self.store, &self.scope, &start).await?;
        self.listing_owed
            .store(false, std::sync::atomic::Ordering::Relaxed);
        let report = self
            .run(crate::page_walk::WatermarkCursor::open(
                held,
                banked_replica,
                start,
                pinned,
            ))
            .await?;
        // A walk from the stored frontier is a listing too: what it does not
        // serve, this replica already holds.
        self.listing_ran(&report, false).await?;
        Ok(report)
    }

    /// Backstop 2 — the per-entry full-state reconcile: the same walk from a
    /// **zero** frontier, which the feed answers with every live entry (one row
    /// per `(item, writer)`) rather than a journal replay, because a put
    /// collapses its own writer's predecessors.
    ///
    /// Converges a replica that missed compacted journal rows, and re-presents
    /// rows an earlier walk left [`WalkReport::unopened`]. It sends no
    /// watermark; on the nest leg it still pages by the echoes it is sent —
    /// which is what keeps a scope whose live rows name more writers than the
    /// ceiling reconcilable — and banks them.
    ///
    /// Its answer is also this pass's listing ([`Self::listing`]): cleared
    /// as the walk opens, set only when it completes.
    pub async fn reconcile(&self) -> Result<WalkReport> {
        if !self.publish_to_feed {
            return self.run(BTreeMap::new()).await;
        }
        *self
            .listing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        *self
            .collecting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(BTreeMap::new());
        *self
            .served
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        *self
            .collecting_served
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(BTreeMap::new());
        *self
            .inherited_rows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        *self
            .collecting_inherited
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Vec::new());
        *self
            .unkeyed_predecessor_mints
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        *self
            .collecting_unkeyed_predecessor_mints
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Vec::new());
        self.listing_owed
            .store(false, std::sync::atomic::Ordering::Relaxed);
        let walked = self
            .run(crate::page_walk::WatermarkCursor::open(
                None,
                None,
                BTreeMap::new(),
                BTreeMap::new(),
            ))
            .await;
        let collected = self
            .collecting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let inherited = self
            .collecting_inherited
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let served = self
            .collecting_served
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let unkeyed_mints = self
            .collecting_unkeyed_predecessor_mints
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if walked.is_ok() {
            *self
                .unkeyed_predecessor_mints
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = unkeyed_mints;
            *self
                .listing
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = collected;
            *self
                .served
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = served;
            *self
                .inherited_rows
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = inherited;
        }
        if let Ok(report) = &walked {
            self.listing_ran(report, true).await?;
        }
        walked
    }

    /// `cursor` is the frontier sent with the next page — and it is **not** the
    /// stored frontier.
    ///
    /// The stored frontier only ever records rows this replica holds (the
    /// store's accounting law refuses to advance past a row it does not have),
    /// so a row skipped as [`WalkReport::unopened`] cannot advance it. Left at
    /// that, a page whose rows are all unopenable would be re-served forever.
    /// The in-walk cursor tracks the highest seq *seen* per writer instead, so
    /// paging always moves; nothing about it is persisted, which is what keeps
    /// the durable frontier honest and lets [`Self::reconcile`] come back to
    /// those rows later.
    ///
    /// On the nest leg the cursor is a [`crate::page_walk::WatermarkCursor`],
    /// and the in-walk cursor is additionally a *projection*: each echo drops
    /// the writers its watermark covers. That is still only paging — the
    /// stored frontier this walk advances through [`Self::apply`] stays
    /// complete.
    async fn run<C: crate::page_walk::FrontierCursor>(&self, cursor: C) -> Result<WalkReport> {
        // Per-walk memo of opened generations: id → derived schedule. KEM
        // decap + schedule derivation happen once per generation per walk,
        // not once per row. Positive entries only — a generation this device
        // cannot key yet is re-tried per row, so a top-up wrap applied
        // EARLIER in the same walk serves the rows after it (a negative memo
        // would hide it until the next reconcile).
        //
        // It lives in the apply closure's captures rather than in a parameter
        // the driver threads: that is what lets this walk and the group
        // plane's — whose `apply` wants no memo at all — share one page loop
        // despite the arity difference.
        let mut generation_schedules: BTreeMap<[u8; 32], FleetOnlySchedule> = BTreeMap::new();
        // An `ext:<kind>` walk's writer authority, read once as it opens
        // (`crate::ext_writers`).
        if self.ext_kind().is_some() {
            let writers =
                crate::ext_writers::ExtWriters::read(self.store, &self.trust.root).await?;
            *self.ext_writers.lock().expect("ext writers poisoned") = Some(writers);
        }
        // The paging law itself (empty page = converged; count; snapshot;
        // apply; take the echo; refuse to spin) is `crate::page_walk`'s.
        crate::page_walk::drive_checkpointed(
            cursor,
            WalkReport::default(),
            "account-state walk",
            async |cursor: &C| {
                let reply: SyncChangesListReply = self
                    .rpc
                    .request(
                        "fauna.sync.changes.list",
                        SyncChangesListRequest {
                            since: NEST_SLOT_UNUSED,
                            item_class: Some(ItemClass::StateEntry.as_wire().to_string()),
                            scope: Some(self.scope.clone()),
                            frontier: Some(cursor.frontier().clone()),
                            held_through_seq: cursor.held_through_seq(),
                            // The retention gate's mark (charter § The
                            // generation machinery → *Fleet-scope
                            // reclamation*, clause (1)): naming ourselves
                            // beside the watermark we bank is what lets the
                            // nest hold every row we have not walked past
                            // yet. Nest leg only — a peer keeps no marks.
                            walker_id: self.bound().then(|| self.store.writer().to_hex()),
                            ..Default::default()
                        },
                    )
                    .await
                    .map_err(|e| anyhow::anyhow!("fauna.sync.changes.list (state-entry): {e}"))?;
                // The gate's watermark rides every page; the last page's
                // value — stamped after this walker's own mark landed — is
                // what the reclamation step behind this walk reads.
                self.note_retirable_through(reply.retirable_through_seq);
                self.void_coordinates_with_the_bank(cursor, &reply).await?;
                Ok(reply)
            },
            async |change: &SyncChange, report: &mut WalkReport, cursor: &mut C| {
                self.apply(change, report, cursor, &mut generation_schedules)
                    .await
            },
            // The one durable write the walk makes beside the rows it applies:
            // the watermark, banked past the page's applies and past the
            // refusals, so it never claims a page the walk did not take. A
            // cursor with no watermark — the peer leg's, or a nest leg's
            // against a nest that has never echoed — banks nothing. The bank
            // is keyed by the replica that echoed it, so a reconcile's echo
            // from another replica replaces a stale bank rather than
            // max-merging under it. A nest leg whose bank this walk found
            // void (another replica, or unconfirmed — § The bind leg, ruling
            // 2) instead CLEARS the persisted watermark, so the next walk
            // reopens with the whole stored frontier rather than narrow.
            async |cursor: &C| {
                // A linked nest pages by its own echoes inside the walk and
                // is never banked: the one bank there is is the bound nest's.
                if self.linked {
                    return Ok(());
                }
                if let Some(held) = cursor.held_through_seq() {
                    let held =
                        u64::try_from(held).context("account-state walk: negative watermark")?;
                    self.store
                        .raise_nest_watermark(&self.scope, cursor.replica_id(), held)
                        .await?;
                } else if cursor.watermark_dishonoured() {
                    self.store.clear_nest_watermark(&self.scope).await?;
                }
                Ok(())
            },
        )
        .await
    }

    /// **A relay row's serve coordinate is void whenever the watermark is**
    /// (`account-sync-plane.md` § The bind leg, ruling 2): both are positions
    /// in the log of the replica the bank was recorded from. Run on every
    /// nest-leg reply BEFORE its rows are applied — the walk stamps each row's
    /// coordinate as it applies it, so a void placed at the bank, after the
    /// page, would wipe what the new replica's first page just stamped. The
    /// reply voids the bank when it names another replica than the persisted
    /// one (or none where one is kept, or the reverse) — which a reconcile
    /// meets too, sending no watermark of its own — or when the cursor finds
    /// the inherited bank unconfirmed ([`crate::page_walk::FrontierCursor::voids_bank`]).
    /// Once voided nothing is banked, so the rest of the walk passes here
    /// untouched until its own echo banks afresh.
    async fn void_coordinates_with_the_bank<C: crate::page_walk::FrontierCursor>(
        &self,
        cursor: &C,
        reply: &SyncChangesListReply,
    ) -> Result<()> {
        if !self.bound() || self.store.nest_watermark(&self.scope).await?.is_none() {
            return Ok(());
        }
        let banked = self.store.nest_watermark_replica(&self.scope).await?;
        let named = reply.replica_id.as_ref().map(|id| id.to_vec());
        if banked != named || cursor.voids_bank(reply) {
            self.store.void_nest_watermark(&self.scope).await?;
        }
        Ok(())
    }

    /// Refuse a served row under this store's CURRENT writer that this
    /// journal cannot vouch for — the burnt-journal signature
    /// (`account-replica-posture.md` § The store device principal,
    /// refinement 11). Every arm reaches this on an OPENED row: the two that
    /// compare plaintext hold it already, and the two that reason from the
    /// coordinates alone come through [`Self::refuse_burnt_if_it_opens`],
    /// which opens first so a rotation never fires on nest-asserted
    /// metadata. Not ingested, not echoed, the frontier left where it
    /// was; counted in [`WalkReport::own_burnt`] and stamped durably
    /// ([`AccountStore::mark_writer_burnt`]) so the pump's reassembly and the
    /// assembly's heal arm can rotate the writer away — this handle holds
    /// neither the slot nor the migration section, so the refusal is all a
    /// walk may do.
    ///
    /// One more thing it does, at the verdict rather than first at the heal's
    /// compaction: the relay plane at this coordinate becomes the fleet's
    /// word — whatever this replica recorded there (a row the nest refused,
    /// or never saw) is retired and the served row recorded in its place —
    /// so no peer meets the burnt row in the window before the rotation
    /// (refinement 11 → *a refused row's relay residue*; the carry arm does
    /// the same for the RETIRED burnt writer).
    #[allow(clippy::too_many_arguments)] // the served row's three parts ride beside the verdict
    async fn refuse_burnt(
        &self,
        writer: &WriterId,
        origin_seq: u64,
        how: &str,
        item_key: &[u8],
        op: &str,
        envelope: &[u8],
        report: &mut WalkReport,
    ) -> Result<()> {
        self.store
            .retire_relay_rows_at(&self.scope, writer, origin_seq)
            .await
            .context("account-state walk: retiring the burnt coordinate's relay rows")?;
        self.store
            .record_relay_row(&RelayRow {
                scope: self.scope.clone(),
                item_class: ItemClass::StateEntry.as_wire().to_string(),
                writer: *writer,
                writer_seq: origin_seq,
                item_key: item_key.to_vec(),
                op: op.to_string(),
                entry: Some(envelope.to_vec()),
                feed_seq: None,
            })
            .await
            .context("account-state walk: relay plane (the fleet's row at a burnt coordinate)")?;
        tracing::warn!(
            writer = %writer.to_hex(),
            seq = origin_seq,
            scope = %self.scope,
            "account-state walk: a feed holds a row under this store's own writer {how} — \
             this journal is not the one that authored it (a store dir restored from an \
             older backup, or a slot reused over a fresh dir), so the writer is BURNT: \
             refusing the row and marking the writer for rotation (charter § The store \
             device principal, refinement 11)"
        );
        self.store.mark_writer_burnt(writer).await?;
        report.own_burnt += 1;
        Ok(())
    }

    /// **Open before you burn** — the burnt verdict on the two arms that have
    /// nothing but the wire row's COORDINATES to reason from.
    ///
    /// `origin_writer`/`origin_seq` are nest-asserted feed metadata: nothing
    /// signs them, and a same-account sibling may put under this writer's id
    /// (the plane's standing trust). The other two arms of the signature
    /// compare the served row's PLAINTEXT against what the journal and the
    /// entry hold, so by the time they rule, [`Self::trial_open`] has already
    /// put the row through the in-seal writer signature (R13,
    /// `fauna_core::account_entry_crypto`) — a row that opens there was
    /// sealed by a journal holding this writer's key, and only a previous life
    /// of THIS store qualifies. These two arms — a row above everything the
    /// journal holds, and a row at a coordinate the journal holds nothing at —
    /// have nothing to compare with, so the open IS the whole evidence and it
    /// has to come first.
    ///
    /// A row that does not open was sealed by nobody this device can name: a
    /// forgery, or bytes corrupted in flight. It is accounted
    /// [`WalkReport::unopened`] and NOTHING else happens — the writer is not
    /// stamped burnt (refinement 11 arm (b): *a rotation is a heavy act and
    /// fires on positive evidence only*, the same conservatism the
    /// held-coordinate arms already apply to an unopenable row), and the
    /// frontier is left exactly where the burnt verdict would have left it, so
    /// the row stays served and counted. Ruling on the coordinates alone let
    /// the nest by itself force a writer rotation — a reassembly and a fresh
    /// enrollment once per runtime worker, once more per relaunch — by serving
    /// a single garbage row under our own writer's id.
    ///
    /// The unopened row is also NOT recorded into the relay plane. That is the
    /// one carve-out to the plane's otherwise unconditional
    /// non-editorializing rule ([`Self::take`]), and it is narrow by
    /// construction: only a row served under this replica's OWN CURRENT
    /// writer, at a coordinate this journal can place nobody at, which opens
    /// under no key this device holds. Relaying another writer's unopenable
    /// row is the honest mirror of what the nest serves, and some reader
    /// somewhere holds the key; relaying THIS one would have this replica
    /// assert a forgery to every peer as its own writer's word, and no reader
    /// anywhere can ever vouch for it.
    #[allow(clippy::too_many_arguments)] // the served row's three parts ride beside the verdict
    async fn refuse_burnt_if_it_opens(
        &self,
        writer: &WriterId,
        origin_seq: u64,
        how: &str,
        item_key: &[u8; 32],
        op: &str,
        envelope: &[u8],
        report: &mut WalkReport,
        generation_schedules: &mut BTreeMap<[u8; 32], FleetOnlySchedule>,
    ) -> Result<()> {
        let coords = EntryCoordinates {
            writer_id: writer.0,
            writer_seq: origin_seq,
            scope: &self.scope,
        };
        if self
            .trial_open(
                &coords,
                item_key,
                envelope,
                generation_schedules,
                &mut report.unkeyed,
            )
            .await?
            .is_none()
        {
            tracing::warn!(
                writer = %writer.to_hex(),
                seq = origin_seq,
                scope = %self.scope,
                "account-state walk: a feed holds a row under this store's own writer {how}, and \
                 it opens under no key this device holds — a forgery or a corrupted row, NOT the \
                 burnt-journal signature: accounted unopened, the writer left alone and the row \
                 not relayed onward (charter § The store device principal, refinement 11)"
            );
            report.unopened += 1;
            return Ok(());
        }
        self.refuse_burnt(writer, origin_seq, how, item_key, op, envelope, report)
            .await
    }

    async fn apply<C: crate::page_walk::FrontierCursor>(
        &self,
        change: &SyncChange,
        report: &mut WalkReport,
        cursor: &mut C,
        generation_schedules: &mut BTreeMap<[u8; 32], FleetOnlySchedule>,
    ) -> Result<()> {
        report.rows += 1;
        let (writer, origin_seq) = row_coordinates(change)?;
        // Seen — whatever happens below, this page has shown us this row.
        cursor.saw(writer.to_hex(), origin_seq as i64);

        // Our own row, coming back — under the CURRENT writer, or under a
        // RETIRED one (principal succession: the machine's former identity's
        // rows live on the feed forever, and this store holds them in their
        // locally-AUTHORED form, whose `item_ref` carries the local entry
        // counter where an ingest would re-derive the wire form with
        // `entry_version = origin_seq` — re-ingesting them false-equivocates
        // against our own history; V13, 2026-08-15). Either way we hold it,
        // so the only thing owed is the accounting. (A replica that has
        // *lost* the row — restored from a store backup that predates it —
        // falls through and ingests it like any other writer's.)
        //
        // ⚠ Peer-leg exception, CURRENT writer only: our own frontier slot
        // doubles as the **published-to-the-nest high-water**
        // ([`Self::publish_pending`]'s watermark), and a PEER echoing our row
        // back proves only that the peer holds it — advancing the slot here
        // would silently convince the nest leg the row was published, and it
        // would never reach the nest. Every OTHER writer's slot — retired
        // ones included: nothing publishes as them again — stays
        // transport-independent ("applied through" is true however the row
        // arrived).
        //
        // ⚠ Read LIVE ([`AccountStore::writer_relation`]), never
        // `store.writer()` / `store.retired_writers()`: those are `open`-time
        // snapshots, and `rotate_writer_identity` takes the backend, so a
        // co-located sibling's fence `A -> B` leaves this handle answering
        // `Foreign` for `B` — the store's OWN current writer. The exception
        // then does not fire and the row falls through to ingest, whose tail
        // advances `B`'s slot unconditionally: the watermark says published
        // when nothing was, and the frontier is MAX-merge, so it never
        // regresses (V13's own root cause, one consumer
        // further on). The equivocation `bail!` in `ingest_state` is the same
        // misclassification's other branch — it needs no separate remedy,
        // because a row correctly recognized as our own never reaches ingest.
        let relation = self.store.writer_relation(&writer).await?;
        let own = relation == WriterRelation::Current;
        let envelope = change
            .entry
            .as_ref()
            .with_context(|| format!("state-entry row at seq {} carries no entry", change.seq))?;
        let item_key = item_key_of(change)?;
        self.note_listed(
            &writer,
            &item_key,
            origin_seq,
            self.feed_coordinate_of(change.seq),
        );
        let held = if relation.is_own() {
            self.store.max_held_seq(&self.scope, &writer).await?
        } else {
            None
        };
        if own && !held.is_some_and(|held| held >= origin_seq) {
            // The CURRENT writer's row above everything this journal holds
            // under it. A writer lives exactly as long as its journal
            // (`account-replica-posture.md` § The store device principal,
            // refinement 11), so a journal holds every seq its writer ever
            // issued — the pre-ruling reading, "a replica restored from a
            // store backup that predates the row falls through and ingests
            // it", is exactly the burnt case: the coordinates above the
            // backup's high-water were used by the pre-backup life, and the
            // next local put would reuse them. Refused, never ingested (an
            // ingest would seed `next_local_seq` past them by accident and
            // leave the wire-form row where the journal expects its own),
            // and the frontier deliberately does NOT advance past it, so
            // the row stays served — and counted — until the heal lands.
            //
            // The coordinates that put us in this arm are the NEST's word,
            // so the envelope is opened before the verdict lands
            // ([`Self::refuse_burnt_if_it_opens`]): a row that opens under
            // this writer's own in-seal signature is a previous life's, and
            // one that does not is a forgery nobody sealed.
            return self
                .refuse_burnt_if_it_opens(
                    &writer,
                    origin_seq,
                    "above every row this journal holds",
                    &item_key,
                    &change.change_type,
                    envelope,
                    report,
                    generation_schedules,
                )
                .await;
        }
        // The carry arm (refinement 11 → *the retired burnt writer's rows
        // are carried, never echoed*): a RETIRED writer whose journal the
        // walk found burnt — `burnt_writer_id` outlives the fence and names
        // it — holds coordinates that vouch for nothing. The row this store
        // holds at a held coordinate is the burnt life's, which the fleet may
        // serve under another item, under the same item with another value,
        // or not at all (refused, and left below the slot by a pre-ordering
        // build's out-of-order publish — residue (i); or re-put by a
        // pre-compaction heal and never compacted — residue (ii)). Echoing
        // would drop the fleet's row there for good, at this replica only;
        // ingesting would equivocate against our own history (V13); a
        // compact-and-re-put would loop against a feed serving both rows (a
        // peer still relaying the burnt life's). So the served value is CARRIED: merged through
        // the ordinary class-2 apply and, when it changes the entry,
        // re-journaled as this replica's own row — idempotent, so every
        // re-presentation after the first is a no-op — with nothing written
        // at the retired coordinate. A coordinate the burnt journal holds
        // NOTHING at (freed by the heal's compaction) is ordinary retired
        // history and ingests. The verdict is read live, per row, for the
        // same reason `writer_relation` is (the fence and the walk may run in
        // different handles); it costs one meta read only on a retired
        // writer's held coordinate.
        let burnt_retired_at_held = if relation == WriterRelation::Retired
            && held.is_some_and(|held| held >= origin_seq)
            && self.store.burnt_writer().await?.as_ref() == Some(&writer)
        {
            Some(
                self.store
                    .scope_rows(&self.scope, &writer, origin_seq.saturating_sub(1), 1)
                    .await?
                    .into_iter()
                    .any(|row| row.seq == origin_seq),
            )
        } else {
            None
        };
        let served = Served {
            change,
            writer,
            origin_seq,
            item_key,
            envelope,
        };
        match burnt_retired_at_held {
            Some(true) => {
                return self
                    .take(
                        served,
                        report,
                        generation_schedules,
                        Take::Carry(Carried::RetiredBurnt),
                    )
                    .await;
            }
            Some(false) => {
                return self
                    .take(served, report, generation_schedules, Take::Ingest)
                    .await;
            }
            None => {}
        }

        if relation.is_own() && held.is_some_and(|held| held >= origin_seq) {
            if own {
                // A held coordinate is this journal's own echo only when the
                // row it holds there is the same item — and, while that row
                // is still the entry's latest write, the same content.
                // Under a different item it is one burnt shape: the journal
                // reused a coordinate a previous life had already
                // published. Under the SAME item with other content it is
                // the other, and the commonest restore shape of all — a
                // backup whose next writes hit the keys the previous life
                // also wrote after it, leaving every served coordinate held
                // and every item matching, so neither of the other arms
                // sees anything. The comparison needs the served row's
                // plaintext, which is why an unopenable own row is left as
                // the echo it was read as before the ruling — a rotation is
                // a heavy act, and it fires on positive evidence only.
                let at_coordinate = self
                    .store
                    .scope_rows(&self.scope, &writer, origin_seq.saturating_sub(1), 1)
                    .await?
                    .into_iter()
                    .find(|row| row.seq == origin_seq);
                let Some(held_row) = at_coordinate else {
                    // The journal's high-water is at or above this
                    // coordinate and yet nothing sits ON it. That is not by
                    // itself a burnt shape: the writer's seq counter is
                    // cross-scope (`next_local_seq` reads `max_writer_seq`,
                    // unfiltered) while `held` is per-scope, so every seq
                    // this writer spent on the sibling fleet scope is a
                    // legitimate gap here — and a pump pass re-presents
                    // every live row from a ZERO frontier
                    // ([`Self::reconcile`]), so a row planted in one is met
                    // on every pass, not once. Coordinates again, and the
                    // nest's word again: open before the verdict, exactly as
                    // the arm above does.
                    return self
                        .refuse_burnt_if_it_opens(
                            &writer,
                            origin_seq,
                            "at a coordinate this journal holds nothing at",
                            &item_key,
                            &change.change_type,
                            envelope,
                            report,
                            generation_schedules,
                        )
                        .await;
                };
                let coords = EntryCoordinates {
                    writer_id: writer.0,
                    writer_seq: origin_seq,
                    scope: &self.scope,
                };
                if let ItemRef::StateKey {
                    kind,
                    key,
                    entry_version,
                } = &held_row.item
                    // A held coordinate's comparison, not the row's open: a
                    // miss here falls through to the walk's own open, which
                    // names an unkeyed generation once.
                    && let Some(served) = self
                        .trial_open(
                            &coords,
                            &item_key,
                            envelope,
                            generation_schedules,
                            &mut Default::default(),
                        )
                        .await?
                {
                    if (&served.kind, &served.key) != (kind, key) {
                        return self
                            .refuse_burnt(
                                &writer,
                                origin_seq,
                                "at a coordinate this journal holds under a different item",
                                &item_key,
                                &change.change_type,
                                envelope,
                                report,
                            )
                            .await;
                    }
                    // Same item. The journal row carries no value, but the
                    // ENTRY does: [`Self::publish_pending`] re-seals an own
                    // row from `entry_to_plaintext(&current)` for exactly as
                    // long as `entry_version` agrees, so a row that is still
                    // the entry's latest write sealed precisely what the
                    // entry holds now. Anything else served there was
                    // authored by another journal — positive evidence. Every
                    // path that rewrites an entry bumps `entry_version`
                    // (`put_state`, `ingest_state`, a tombstone), so a true
                    // echo of the entry's latest row matches whole.
                    //
                    // No evidence, all echoes, matching the unopenable-row
                    // conservatism above: a row the entry has moved past
                    // (the later write's plaintext is not this row's), an
                    // item the entry no longer holds at all, and — by the
                    // pattern this arm is inside — a class-1 `Cid` at the
                    // coordinate.
                    if let Some(current) = self.store.state(kind, key).await?
                        && current.entry_version == *entry_version
                        && served != entry_to_plaintext(&current)
                    {
                        return self
                            .refuse_burnt(
                                &writer,
                                origin_seq,
                                "at a coordinate this journal holds under the same item with \
                                 other content",
                                &item_key,
                                &change.change_type,
                                envelope,
                                report,
                            )
                            .await;
                    }
                }
            }
            if !own || self.bound() {
                self.store
                    .advance_frontier(&self.scope, &writer, origin_seq)
                    .await?;
            }
            if own && self.parks_refused_for_room() {
                // A parked row the nest now serves — a sibling's diff pushed
                // it — is settled (refinement 11 → *A row refused for room is
                // parked*: the own echo is one of its three exits).
                self.store
                    .unpark(&self.scope, &writer, origin_seq)
                    .await
                    .context("account-state walk: unparking an own echo")?;
            }
            // The nest holds this row of ours verbatim, so the relay plane
            // holds it too: an upsert the store's newer-seq guard makes a
            // no-op whenever the row is already there, and the repair when it
            // is not — a publish the nest accepted but whose reply was lost
            // was re-sent, refused `stale_writer_seq` as a replay, and its
            // relay row retired at the refusal (refinement 11 → *a refused
            // row's relay residue*); this echo is the proof the nest holds it.
            self.store
                .record_relay_row(&RelayRow {
                    scope: self.scope.clone(),
                    item_class: ItemClass::StateEntry.as_wire().to_string(),
                    writer,
                    writer_seq: origin_seq,
                    item_key: item_key.to_vec(),
                    op: change.change_type.clone(),
                    entry: Some(envelope.to_vec()),
                    // The echo is also how a row published before the reply
                    // stamped it learns its feed coordinate.
                    feed_seq: self.feed_coordinate(change.seq),
                })
                .await
                .context("account-state walk: relay plane (own echo)")?;
            report.self_echo += 1;
            return Ok(());
        }

        self.take(served, report, generation_schedules, Take::Ingest)
            .await
    }

    /// Take a served row past the own/retired arms: relay it, open it, run
    /// the merge policy and land the outcome — journaled at its coordinate
    /// ([`Take::Ingest`]) or carried without one ([`Take::Carry`]).
    async fn take(
        &self,
        served: Served<'_>,
        report: &mut WalkReport,
        generation_schedules: &mut BTreeMap<[u8; 32], FleetOnlySchedule>,
        mode: Take,
    ) -> Result<()> {
        let Served {
            change,
            writer,
            origin_seq,
            item_key,
            envelope,
        } = served;
        if mode == Take::Carry(Carried::RetiredBurnt) {
            // The burnt life's relay row at this coordinate under another
            // item is not the fleet's row there — retired before the fleet's
            // is recorded, so a peer is served exactly what the nest serves.
            // (The double-served arm below retires nothing: BOTH its rows are
            // the nest's, and a peer must meet the same shape.)
            let retired = self
                .store
                .retire_shadowed_relay_rows(&self.scope, &writer, origin_seq, &item_key)
                .await?;
            if retired > 0 {
                tracing::warn!(
                    writer = %writer.to_hex(),
                    seq = origin_seq,
                    scope = %self.scope,
                    retired,
                    "account-state walk: a retired burnt writer's relay row at a coordinate the \
                     fleet serves under another item is retired for the fleet's (refinement 11)"
                );
            }
        }

        // The relay plane (W2.6): keep the verbatim wire row so this replica
        // can serve it onward peer-wise — R6's "verbatim relay of other
        // writers' rows". Deliberately UNCONDITIONAL for every
        // coordinate-valid row, `unopened` and `unmergeable` included: a relay
        // does not editorialize (the nest serves those rows to every replica
        // too, and a key-less custodian could not tell them apart anyway) —
        // each reader makes its own skip decision, and withholding a row here
        // would only make peers' views diverge from the nest's. The ONE
        // carve-out never reaches this point: an unopenable row served under
        // our OWN CURRENT writer at a coordinate this journal cannot place
        // anybody at returns from [`Self::refuse_burnt_if_it_opens`] without
        // a relay row, because relaying it would assert a forgery to peers as
        // this writer's own word rather than mirror the nest. Forgetting a
        // recorded row is a separate, later act (the reclamation pass's
        // dead-everywhere forgets, idempotent across a re-serve) and never
        // happens here.
        let served_relay = RelayRow {
            scope: self.scope.clone(),
            item_class: ItemClass::StateEntry.as_wire().to_string(),
            writer,
            writer_seq: origin_seq,
            item_key: item_key.to_vec(),
            op: change.change_type.clone(),
            entry: Some(envelope.to_vec()),
            feed_seq: self.feed_coordinate(change.seq),
        };
        self.store
            .record_relay_row(&served_relay)
            .await
            .context("account-state walk: relay plane")?;

        let coords = EntryCoordinates {
            writer_id: writer.0,
            writer_seq: origin_seq,
            scope: &self.scope,
        };
        // Set when the row opened under a predecessor's DELEGABLE schedule —
        // the retire behind the carry's candidate, once it merges. A mint row
        // the predecessor's mint keys open is the fleet scope's, never one.
        let mut opened_inherited = false;
        let (plaintext, mode) = match self
            .trial_open(
                &coords,
                &item_key,
                envelope,
                generation_schedules,
                &mut report.unkeyed,
            )
            .await?
        {
            Some(plaintext) => (plaintext, mode),
            // Not ours to open — an attested predecessor's, perhaps: its
            // delegable rows cross the succession here, carried
            // (`succession-aftermath.md` § Re-key scope), and so does the
            // mint record of a generation this device keys
            // (`owner-key-material.md` § Path A-sibling-2 → *Rotation*, the
            // succession rider). A row already in a carry arm stays in it;
            // the inherited row then lands exactly as those do, and the
            // double-served check below is skipped with them — nothing is
            // journaled at this coordinate either way.
            None => match self
                .trial_open_inherited(&coords, &item_key, envelope)
                .inspect(|_| opened_inherited = true)
                .or_else(|| self.trial_open_predecessor_mint(&coords, &item_key, envelope))
            {
                Some(plaintext) => (
                    plaintext,
                    match mode {
                        Take::Ingest => Take::Carry(Carried::Inherited),
                        carry @ Take::Carry(_) => carry,
                    },
                ),
                None => {
                    report.unopened += 1;
                    return Ok(());
                }
            },
        };

        // Defense in depth over the AAD: the cleartext op a key-less custodian
        // reads must agree with the tombstone marker inside the seal. The seal
        // is authoritative — a relay cannot forge one — so a disagreement means
        // the cleartext half was rewritten in flight, and we refuse rather than
        // pick a side.
        let claimed_tombstone = change.change_type == OP_TOMBSTONE;
        if claimed_tombstone != plaintext.tombstone {
            bail!(
                "state-entry row at seq {}: cleartext op {:?} disagrees with the sealed \
                 tombstone marker ({})",
                change.seq,
                change.change_type,
                plaintext.tombstone
            );
        }

        // The A5 partition, read side (step 7): the door refuses to *originate*
        // a kind outside its home scope; this is the mirror that stops a row
        // which reached the feed anyway from being applied off-partition. Same
        // skip-never-abort discipline as an unmergeable row, and the same
        // author set — any fleet-key holder, a removed device included — since
        // an honest writer's door would not have let it out.
        if let Some(home) = home_scope_for_kind(&plaintext.kind)
            && home != self.scope
        {
            tracing::warn!(
                kind = %plaintext.kind,
                home_scope = %home,
                walked_scope = %self.scope,
                "account-state walk: skipping a row riding a scope its kind never seals into \
                 (A5 partition, account-data-taxonomy.md § The generation machinery)"
            );
            report.unmergeable += 1;
            return Ok(());
        }

        // Write authority over a third-party kind (`third-party-kinds.md`
        // § Principal write authority, position (2)): the seal verified the
        // writer's signature, which any holder of the kind's delegable pair
        // can produce; whether that writer was AUTHORIZED is this check — one
        // of the account's devices, or a key the owner's grant log ever
        // confined a `content.write` tuple over this kind to. Skipped and
        // counted, never fatal, and re-presented: the frontier does not pass
        // it, so a row that beat its grant event here is admitted later.
        if let Some(writers) = self
            .ext_writers
            .lock()
            .expect("ext writers poisoned")
            .as_ref()
            && writer != self.store.writer()
            && !writers.admits(&writer.0, &plaintext.kind)
        {
            tracing::warn!(
                writer = %writer.to_hex(),
                kind = %plaintext.kind,
                scope = %self.scope,
                "account-state walk: skipping a third-party-kind row from a writer that is \
                 neither one of the account's devices nor granted content.write over the kind \
                 (third-party-kinds.md § Principal write authority)"
            );
            report.unmergeable += 1;
            return Ok(());
        }

        // The double-served coordinate (refinement 11 → *a foreign writer's
        // second row at a held coordinate is carried*): the nest refuses a
        // reused `(scope, writer, seq)`, but a peer that pulled a burnt
        // writer's row before the nest refused its coordinate relays it for
        // good beside the nest's row, and every replica but the healed one
        // meets the two as a FOREIGN writer's rows — the first
        // ingested, the second used to hit `ingest_state`'s equivocation
        // refusal and abort the page. The incremental walk self-cleared (the
        // first row's frontier advance gates both off the next page), but
        // the full pass's zero-frontier reconcile met it again every pass,
        // forever. The refusal buys class-2 no security (a second row at a
        // coordinate must be AAD-bound to it, so only a fleet-key holder can
        // produce one — who could write any value at a fresh coordinate and
        // have the merge take it), so a row under a DIFFERENT item at a held
        // coordinate is carried: merged, journaled nowhere, counted apart
        // from the burnt arm so genuine equivocation stays visible. A
        // same-item row with other content at a held coordinate is NOT this
        // (the nest collapses per `(item, writer)`, so that is corruption or
        // a forged relay) and still refuses below. One point read per
        // ingested row, no more than the `state` read beside it.
        let mode = if mode == Take::Ingest
            && self
                .store
                .scope_rows(&self.scope, &writer, origin_seq.saturating_sub(1), 1)
                .await?
                .into_iter()
                .find(|row| row.seq == origin_seq)
                .is_some_and(|held_row| match &held_row.item {
                    ItemRef::StateKey { kind, key, .. } => {
                        (kind, key) != (&plaintext.kind, &plaintext.key)
                    }
                    _ => true,
                }) {
            tracing::warn!(
                writer = %writer.to_hex(),
                seq = origin_seq,
                scope = %self.scope,
                kind = %plaintext.kind,
                "account-state walk: the feed serves a second item at a coordinate this journal \
                 already holds — a burnt writer's row a peer relayed beside the nest's, or a \
                 writer that reused a seq; carried, nothing written at the coordinate (refinement 11)"
            );
            Take::Carry(Carried::DoubleServed)
        } else {
            mode
        };

        let policy = self
            .kinds()
            .merge_policy(&plaintext.kind)
            .with_context(|| {
                format!(
                    "opened a {:?} entry under this build's own key schedule but the kind has no \
                 merge policy — the trial-open set and the policy table have drifted apart",
                    plaintext.kind
                )
            })?;
        let current = self
            .store
            .state(&plaintext.kind, &plaintext.key)
            .await?
            .map(|e| entry_to_plaintext(&e));

        let row = JournalRow {
            writer,
            seq: origin_seq,
            scope: self.scope.clone(),
            op: if plaintext.tombstone {
                JournalOp::Tombstone
            } else {
                JournalOp::StatePut
            },
            item: ItemRef::StateKey {
                kind: plaintext.kind.clone(),
                key: plaintext.key.clone(),
                // The origin's seq, per `ItemRef::StateKey`'s contract for an
                // ingested row: derived from the wire, so a replay of this same
                // row is byte-identical and therefore idempotent.
                entry_version: origin_seq,
            },
        };

        let outcome = match apply_class2(policy, current.as_ref(), &plaintext) {
            Ok(outcome) => outcome,
            // A row-content refusal must not abort the walk — see
            // [`WalkReport::unmergeable`] and `MergeError::is_row_content`.
            // The row is permanent (the nest collapses per
            // `(item_key, writer)`) and any fleet-key holder can seal one, so
            // a fatal reading of it would wedge this replica's whole plane,
            // not just this item; the frontier deliberately does NOT advance
            // past a row we did not account, matching the `unopened` skip.
            // Build/store inconsistencies still abort — they are our defect.
            Err(err) if err.is_row_content() => {
                tracing::warn!(
                    writer = %hex::encode(writer.0),
                    seq = origin_seq,
                    kind = %plaintext.kind,
                    "skipping an unmergeable account-state row: {err}"
                );
                report.unmergeable += 1;
                return Ok(());
            }
            Err(err) => return Err(err.into()),
        };
        // The retire behind the carry's candidate (`succession-aftermath.md`
        // § Re-key scope → *The predecessor's own row is retired behind the
        // carry*, clause (1)): a row opened under a predecessor's delegable
        // schedule that merged, whatever the merge did with it. Noted by each
        // arm once the row is past every skip.
        let candidate = opened_inherited.then(|| InheritedRow {
            writer,
            writer_seq: origin_seq,
            item_key,
            feed_seq: self.feed_coordinate(change.seq),
            kind: plaintext.kind.clone(),
            key: plaintext.key.clone(),
        });
        match (outcome, mode) {
            (MergeOutcome::KeepCurrent, Take::Ingest) => {
                self.store.ingest_row(&row).await?;
                report.kept += 1;
            }
            (MergeOutcome::KeepCurrent, Take::Carry(why)) => {
                // The entry already holds it (the second and every later
                // presentation of a carried row lands here): nothing to
                // write, and nothing at the held coordinate ever.
                report.count_carry(why);
                self.note_inherited(candidate);
            }
            (MergeOutcome::Replace, Take::Ingest) => {
                self.store
                    .ingest_state(&row, self.entry_of(&plaintext))
                    .await?;
                report.applied += 1;
                self.drop_retained_if_shredded(&plaintext);
            }
            (MergeOutcome::Replace, Take::Carry(why)) => {
                // Carried: the fleet's value, `merge_meta` verbatim, becomes
                // this replica's own row — the tail re-author's re-put shape
                // — and publishes in order. The door's cap applies as it
                // does to a merged value: a row no nest accepts must not
                // become a local row the publish pass can only skip.
                let sized = match SizedEntry::size(&plaintext) {
                    Ok(sized) => sized,
                    Err(err) => {
                        tracing::warn!(
                            writer = %hex::encode(writer.0),
                            seq = origin_seq,
                            kind = %plaintext.kind,
                            "skipping a carried row over the entry cap: {err:#}"
                        );
                        report.unmergeable += 1;
                        return Ok(());
                    }
                };
                let our_seq = self.put_own_row(&sized).await?;
                self.name_carried_row(mode, our_seq, &served_relay);
                report.count_carry(why);
                self.note_inherited(candidate);
                // At the cap the carry is counted and the walk goes on
                // (*At the cap*): the own row stays journaled for the next
                // publish, and an aborted walk would bank no listing.
                let published = self.publish_own_rows_through(&plaintext, our_seq).await;
                report.tolerate_carry_scope_full(published)?;
                self.drop_retained_if_shredded(&plaintext);
            }
            (MergeOutcome::Merged(merged), mode) => {
                // The merged value is a NEW value this replica authored, so it
                // passes the door `put` passes before it reaches the journal.
                // A kind's join is what bounds it (the seen-set's budget fold);
                // this is the backstop that keeps a merge no nest would accept
                // from becoming a local row the publish pass could only skip
                // (see `refuse_if_over_entry_cap`). Handled as a row-content
                // refusal: not accounted, counted, re-presented — never
                // journaled.
                let sized = match SizedEntry::size(&merged) {
                    Ok(sized) => sized,
                    Err(err) => {
                        tracing::warn!(
                            writer = %hex::encode(writer.0),
                            seq = origin_seq,
                            kind = %plaintext.kind,
                            "skipping a row whose merge outgrows the entry cap: {err:#}"
                        );
                        report.unmergeable += 1;
                        return Ok(());
                    }
                };
                // The incoming row is consumed as itself (journaled at its
                // coordinate — unless carried, when the coordinate stays the
                // burnt life's); the merged value is a NEW value this replica
                // authored, so it goes on our own log and out to the feed —
                // that is what lets the peer converge on fields only we had.
                if mode == Take::Ingest {
                    self.store.ingest_row(&row).await?;
                }
                self.account_served(mode, &writer, origin_seq).await?;
                let our_seq = self.put_own_row(&sized).await?;
                self.name_carried_row(mode, our_seq, &served_relay);
                let published = self.publish_own_rows_through(&merged, our_seq).await;
                match mode {
                    Take::Ingest => {
                        report.merged += 1;
                        published?;
                    }
                    Take::Carry(why) => {
                        report.count_carry(why);
                        self.note_inherited(candidate);
                        // At the cap, as the `Replace` carry arm above.
                        report.tolerate_carry_scope_full(published)?;
                    }
                }
                self.drop_retained_if_shredded(&merged);
                return Ok(());
            }
            (MergeOutcome::NeedsThreeWay, _) => bail!(
                "kind {:?} needs three-way resolution, which no kind on the plane has wired yet \
                 (conflicts.md owns the machinery)",
                plaintext.kind
            ),
        }
        self.account_served(mode, &writer, origin_seq).await
    }

    /// A carry from a predecessor's row re-authored at `our_seq`: the put
    /// names the predecessor's row (`delegable-scope-reclamation.md`
    /// § Delegable-scope reclamation, part (2)), so it needs no room at a full
    /// scope. The bound nest's delegable plane only — the one walk that
    /// carries.
    fn name_carried_row(&self, mode: Take, our_seq: u64, served: &RelayRow) {
        if mode == Take::Carry(Carried::Inherited)
            && self.bound()
            && self.scope == ACCOUNT_STATE_SCOPE
        {
            self.carry_names
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(our_seq, vec![served.clone()]);
        }
    }

    /// Advance the served writer's frontier past a row [`Self::take`] landed
    /// — except an inherited one. The frontier accounts rows this journal
    /// holds at the writer's coordinates: an ingested row is journaled there,
    /// and the other two carry arms fire only at a coordinate the journal
    /// already holds. A predecessor identity's row is neither — nothing is
    /// written at its writer's coordinate
    /// (`succession-aftermath.md` § Re-key scope; the mint record's carry is
    /// the same shape) — so its writer's frontier
    /// stays where it was and the feed keeps serving the row, exactly as it
    /// did while the row was [`WalkReport::unopened`]; every later
    /// presentation is a `KeepCurrent` that writes nothing.
    async fn account_served(&self, mode: Take, writer: &WriterId, origin_seq: u64) -> Result<()> {
        if mode == Take::Carry(Carried::Inherited) {
            return Ok(());
        }
        self.store
            .advance_frontier(&self.scope, writer, origin_seq)
            .await?;
        Ok(())
    }

    /// The walk-side half of the crypto-shred contract (W5.4a): a mint row
    /// whose applied/merged state is `Shredded` drops the retained bundle's
    /// key the moment it lands — not only when a later read happens to name
    /// that generation (`generation_tip::RetainedKeyCustody`, duty three).
    /// Row content never aborts anything here: an undecodable mint row is the
    /// resolver's business, not this hook's.
    fn drop_retained_if_shredded(&self, state: &EntryPlaintext) {
        let Some(custody) = self.custody else { return };
        if state.kind != fauna_protocol::merge_policy::KIND_GENERATION_MINT || state.tombstone {
            return;
        }
        if let Ok(fauna_core::generation::GenerationMintRecord::Shredded { core, .. }) =
            fauna_core::encoding::canonical_decode(&state.value)
            && let Ok(id) = fauna_core::generation::generation_id(&core)
        {
            custody.drop_generation_key(&id);
        }
    }

    /// Open one feed row under this build's keys, or `None` for the
    /// [`WalkReport::unopened`] skip — naming the row's generation in
    /// `unkeyed` when the skip is for want of that generation's key
    /// ([`WalkReport::unkeyed`]). `Err` is reserved for this replica's own
    /// defects (a store read failing) — row content never aborts.
    async fn trial_open(
        &self,
        coords: &EntryCoordinates<'_>,
        item_key: &[u8; 32],
        envelope: &[u8],
        generation_schedules: &mut BTreeMap<[u8; 32], FleetOnlySchedule>,
        unkeyed: &mut std::collections::BTreeSet<[u8; 32]>,
    ) -> Result<Option<EntryPlaintext>> {
        // v2 (generation-sealed): the cleartext id names which retained
        // generation's schedule opens the row — a lookup, not a trial walk
        // (`peek_generation_id`'s contract); the AEAD tag stays the arbiter,
        // and a lying id simply fails the open. The trial set is the
        // registry's `GenerationTip` kinds: v2 is the generation-sealed form,
        // so a `Gen0` kind's keys can never legitimately open one.
        if let Some(generation) = peek_generation_id(envelope) {
            if let std::collections::btree_map::Entry::Vacant(entry) =
                generation_schedules.entry(generation)
            {
                let Some(key) = generation_tip::generation_key_for(
                    self.store,
                    &generation,
                    self.writer_key,
                    self.custody,
                )
                .await?
                else {
                    // No wrap reaches this device (yet): unopened —
                    // `reconcile` re-presents the row once a top-up lands
                    // (charter: "topped up on first contact") — and the
                    // generation is one this listing left a row unopened
                    // under for want of the key.
                    unkeyed.insert(generation);
                    return Ok(None);
                };
                entry.insert(FleetOnlySchedule::derive_for_generation(&key));
            }
            let schedule = &generation_schedules[&generation];
            let opened = self
                .trial_kinds(|kind| sealing_epoch(kind) == Some(SealingEpoch::GenerationTip))
                .into_iter()
                .find_map(|kind| {
                    open_entry(&schedule.for_kind(kind), coords, item_key, envelope)
                        .ok()
                        .map(|pt| (kind, pt))
                });
            return Ok(opened.map(|(kind, pt)| {
                self.note_trial_hit(kind);
                pt
            }));
        }
        // v1 (gen-0): each kind is tried under ITS OWN registered branch
        // (R13): a fleet-only kind's entries never open under a delegable key
        // and vice versa, so the trial set is the registry's routing, not a
        // sweep of both branches — and, since step 7, restricted to `Gen0`
        // kinds, because **v1 IS the gen-0 form** (charter § The class-2 entry
        // form → form v2: "v1 stays the gen-0 form forever"). Without the
        // filter, R14's severance stops at the writer: any `BackupKey` holder
        // — a *removed* device retains it forever, the stated exposure — can
        // seal a v1 row of a `GenerationTip` kind under the gen-0 fleet branch
        // and have every reader accept it, which is precisely what wrap
        // targeting exists to prevent. The compat set this forecloses is
        // provably EMPTY: the boolean R14 gate (W2.5 item 0)
        // refused every fleet-only origination *before* the only
        // `GenerationTip` kind was even registered (W2.5 item 3),
        // so no build this project ever shipped could originate one.
        // An `ext:<kind>` plane's trial set is its one kind, opened under
        // the overlay's delegable pair — and only when the overlay admitted
        // it: an unadmitted kind's row stays unopened (the compat answer), a
        // `unopened` count, never a guess (`third-party-kinds.md` § The kinds
        // vocabulary → *The registry overlay*).
        if let Some(kind) = self.ext_kind() {
            let Some(keys) = self.kinds().kind_keys(self.schedule, &kind) else {
                return Ok(None);
            };
            return Ok(open_entry(&keys, coords, item_key, envelope).ok());
        }
        let opened = self
            .trial_kinds(|kind| sealing_epoch(kind) != Some(SealingEpoch::GenerationTip))
            .into_iter()
            .find_map(|kind| {
                let keys = self.kinds().kind_keys(self.schedule, kind)?;
                open_entry(&keys, coords, item_key, envelope)
                    .ok()
                    .map(|pt| (kind, pt))
            });
        Ok(opened.map(|(kind, pt)| {
            self.note_trial_hit(kind);
            pt
        }))
    }

    /// Open a row this plane's own keys could not, under an attested
    /// predecessor's generation-0 **delegable** schedule
    /// ([`Self::with_predecessor_schedules`]) — the inherited carry's open
    /// (`succession-aftermath.md` § Re-key scope). `None` on a plane handed
    /// none, for a v2 (generation-sealed) row — how the random generations
    /// cross a succession is the generation rider's, not this — and for a
    /// row no predecessor schedule opens.
    ///
    /// The trial set is the registry's delegable rung, never a hand-list, and
    /// the schedule type holds no fleet branch: a fleet-only kind has no key
    /// here to be tried under. The one fleet-only kind that crosses has its
    /// own keys and its own gate — [`Self::trial_open_predecessor_mint`].
    fn trial_open_inherited(
        &self,
        coords: &EntryCoordinates<'_>,
        item_key: &[u8; 32],
        envelope: &[u8],
    ) -> Option<EntryPlaintext> {
        if self.predecessors.is_empty() || peek_generation_id(envelope).is_some() {
            return None;
        }
        // Gen-0 delegable kinds only: v1 IS the gen-0 form (see
        // [`Self::trial_open`]'s v1 arm), so a `GenerationTip` kind is never
        // tried here whatever rung it sits on.
        let kinds: Vec<&'static str> = delegable_kinds()
            .filter(|kind| sealing_epoch(kind) != Some(SealingEpoch::GenerationTip))
            .collect();
        self.predecessors.iter().find_map(|schedule| {
            kinds.iter().find_map(|kind| {
                open_entry(&schedule.for_kind(kind), coords, item_key, envelope).ok()
            })
        })
    }

    /// Open a row this plane's own keys could not, under an attested
    /// predecessor's generation-0 keys for the mint kind
    /// ([`Self::with_predecessor_mint_keys`]), and answer it only when it may
    /// be carried — the succession rider's two conditions
    /// (`owner-key-material.md` § Path A-sibling-2 → *Rotation* → *What
    /// crosses*):
    ///
    /// - **(a) the record is true.** It decodes, its logical key is the
    ///   content-derived id of its own core (the Key↔id binding every reader
    ///   of a mint row applies), and a `Minted` record's authorship verifies
    ///   ([`fauna_core::generation::verify_mint_authorship`]). A `Shredded`
    ///   record carries no minter signature; it crosses on the binding and
    ///   on (b), and the kind's join then drops the key
    ///   ([`Self::drop_retained_if_shredded`]).
    /// - **(b) the key is in hand.** The retained bundle holds, at that id, a
    ///   key matching the core's commitment. A device carries no record of a
    ///   generation it does not key, and a plane with no custody attached
    ///   carries none at all.
    ///
    /// `None` for everything else — a row no mint-kind key opens, a
    /// tombstone, a record failing either condition — which the caller counts
    /// [`WalkReport::unopened`] and writes nothing for. The retired keys can
    /// be served anything a holder of the retired `BackupKey` sealed before
    /// the ceremony or a peer relays since; the two conditions admit only the
    /// true record of a generation this device had already keyed (the rider
    /// → *Residuals*, (iv)).
    ///
    /// A `Minted` record meeting (a) for which the bundle holds no key is
    /// still declined, and noted for the pass
    /// ([`Self::unkeyed_predecessor_mints`]): the kept wrap's recovery asks
    /// the holder for it, and the next walk carries it once keyed.
    fn trial_open_predecessor_mint(
        &self,
        coords: &EntryCoordinates<'_>,
        item_key: &[u8; 32],
        envelope: &[u8],
    ) -> Option<EntryPlaintext> {
        use fauna_core::generation::{GenerationMintRecord, generation_id, verify_mint_authorship};

        // v1 only: the mint kind is `Gen0` machinery, and v1 IS the gen-0
        // form ([`Self::trial_open`]'s v1 arm).
        if self.predecessor_mint_keys.is_empty() || peek_generation_id(envelope).is_some() {
            return None;
        }
        let custody = self.custody?;
        // `open_entry` checks the sealed kind against the keys' own, so what
        // opens here is a `fauna.state.generation-mint` entry and nothing else.
        let plaintext = self
            .predecessor_mint_keys
            .iter()
            .find_map(|keys| open_entry(keys, coords, item_key, envelope).ok())?;
        if plaintext.tombstone {
            return None;
        }
        let record: GenerationMintRecord =
            fauna_core::encoding::canonical_decode(&plaintext.value).ok()?;
        let core = match &record {
            GenerationMintRecord::Minted { core, .. }
            | GenerationMintRecord::Shredded { core, .. } => core,
        };
        let id = generation_id(core).ok()?;
        if fauna_core::hex32::encode(&id) != plaintext.key {
            return None;
        }
        if let GenerationMintRecord::Minted { minter_sig, .. } = &record {
            verify_mint_authorship(&id, core, minter_sig).ok()?;
        }
        let Some(held) = custody.retained_generation_key(&id) else {
            // (a) met, (b) not: the kept wrap's candidate
            // (`owner-key-material.md` § Path A-sibling-2 → *Rotation*, the
            // succession rider → *The kept wrap*). A shredded generation has
            // no key left to recover.
            if matches!(record, GenerationMintRecord::Minted { .. }) {
                self.note_unkeyed_predecessor_mint(UnkeyedPredecessorMint {
                    generation_id: id,
                    core: core.clone(),
                });
            }
            return None;
        };
        (held.commitment() == core.key_commitment).then_some(plaintext)
    }

    /// The trial-open set for one row, in the order worth trying: the
    /// registry's kinds that pass `admit`, with the kind that opened the
    /// PREVIOUS row first. Every attempt is a full AEAD open of the envelope
    /// — the blind is one-way, so nothing cheaper can rule a kind out — and a
    /// feed serves rows in runs of one kind (a mint's spill, a fleet's reach
    /// updates), so the hint turns a dozen opens per row into about one.
    /// Pure ordering: a miss on the hint falls through to the rest, and the
    /// set is deliberately NOT narrowed to this scope's home kinds — an
    /// off-partition row must still OPEN so the read-side A5 check below can
    /// count it `unmergeable` (a hostile writer's row), not `unopened` (a
    /// kind this build predates).
    fn trial_kinds(&self, admit: impl Fn(&&'static str) -> bool) -> Vec<&'static str> {
        let hint = self.trial_hint.lock().ok().and_then(|h| *h);
        let mut kinds: Vec<&'static str> = class2_kinds().filter(admit).collect();
        if let Some(hint) = hint
            && let Some(at) = kinds.iter().position(|k| *k == hint)
        {
            kinds.swap(0, at);
        }
        kinds
    }

    fn note_trial_hit(&self, kind: &'static str) {
        if let Ok(mut h) = self.trial_hint.lock() {
            *h = Some(kind);
        }
    }

    /// **The plane's one write onto this replica's own log**, and the only
    /// caller of `StateStore::put_state` in this module — pinned by
    /// `the_plane_has_one_sized_write_onto_our_own_log`.
    ///
    /// Taking a [`SizedEntry`] rather than a plaintext is what makes the
    /// per-entry cap unforgettable: an arm that journals a value this replica
    /// authored has no way to reach the store except through a ticket only
    /// [`refuse_if_over_entry_cap`] issues. What each arm does with a refusal
    /// stays the arm's own business — `write_local_and_publish` returns it,
    /// the walk's arms count it in [`WalkReport::unmergeable`] and re-present
    /// the row — which is why
    /// the door hands back the refusal instead of handling it.
    ///
    /// Not the road for another writer's row at its own coordinate: that is
    /// `ingest_state`, which journals what the fleet already holds rather than
    /// minting a row of ours, and answers to the origin writer's cap, not ours.
    async fn put_own_row(&self, sized: &SizedEntry<'_>) -> Result<u64> {
        let (_, seq) = self
            .store
            .put_state(self.entry_of(sized.plaintext()))
            .await?;
        Ok(seq)
    }

    fn entry_of(&self, plaintext: &EntryPlaintext) -> StateEntry {
        StateEntry {
            kind: plaintext.kind.clone(),
            key: plaintext.key.clone(),
            scope: self.scope.clone(),
            value: plaintext.value.to_vec(),
            merge_meta: plaintext.merge_meta.as_ref().map(|m| m.to_vec()),
            entry_version: 0, // reassigned by the store
            tombstone: plaintext.tombstone,
        }
    }
}

// The boolean R14 gate (`refuse_if_r14_gated`) lived here through build steps
// 1–5 and was replaced by [`AccountStatePlane::admit_origination`]'s tip
// resolution in step 6 — "removing this gate is the last step of landing the
// generation schedule" discharged exactly as its own doc demanded, with no
// second gate left to drift from the first (charter § The generation
// machinery → *The sealing-epoch axis, and the gate's real shape*).

/// How many of our own journal rows one [`AccountStatePlane::publish_pending`]
/// page reads.
const PAGE: u32 = 256;

/// Write one class-2 item under a plain last-writer-wins stamp — the shape
/// every "publish this device's row" pump repeats (`custody_rows`'s own doc
/// comment already called its version "mirrored on
/// [`crate::device_endpoints_writer`]'s `put_row`" before this existed): bump
/// today's wall clock into an [`LwwStamp`] under `device_id` and hand it,
/// encoded, to [`AccountStatePlane::put`] as the merge metadata.
/// `scope`'s stored frontier as a paging cursor (writer id → hex, seq as
/// `i64`) — shared by every class-2 plane's [`AccountStatePlane::walk`] /
/// [`crate::group_state_plane::GroupStatePlane::walk`], which hold the same
/// [`AccountStore`] and hand-copied this conversion identically.
pub async fn stored_frontier<B: StoreBackend>(
    store: &AccountStore<B>,
    scope: &str,
) -> Result<BTreeMap<String, i64>> {
    Ok(store
        .frontier(scope)
        .await?
        .into_iter()
        .map(|(w, seq)| (w.to_hex(), seq as i64))
        .collect())
}

/// Seed `cursor`'s own-writer slot past whatever this replica already holds
/// under `scope`, so a peer leg's walk never re-serves our own authored rows
/// back at us. The other hand-copied half of [`stored_frontier`]'s pair.
pub async fn seed_past_own_held<B: StoreBackend>(
    store: &AccountStore<B>,
    scope: &str,
    cursor: &mut BTreeMap<String, i64>,
) -> Result<()> {
    let writer = store.writer();
    if let Some(held) = store.max_held_seq(scope, &writer).await? {
        let slot = cursor.entry(writer.to_hex()).or_insert(0);
        *slot = (*slot).max(held as i64);
    }
    Ok(())
}

/// The writers a nest-leg request projected under a banked watermark must
/// keep naming, each mapped to its stored slot: every FOREIGN writer this
/// store has seen above what it has accounted — its relay-plane high-water
/// ([`AccountStore::relay_high_waters`]; [`AccountStatePlane::apply`] records
/// the relay row for every coordinate-valid row before it tries to open it)
/// above its stored frontier slot, because a row of it was left
/// [`WalkReport::unopened`] or [`WalkReport::unmergeable`].
///
/// This is how a banked watermark avoids claiming a row the store never took
/// (charter § Feeds and cursors → *Compaction is a serve-order watermark*, the
/// *Requester* bullet's built policy): the watermark may pass such a row, but
/// its writer stays named at the slot below it, so the nest's per-writer gate
/// re-presents it on every walk exactly as a whole-frontier request would.
///
/// An own writer never counts. Its relay rows are rows this store AUTHORED,
/// so a high-water above its slot is the published-high-water lag
/// [`AccountStatePlane::publish_pending`] works off, never an unheld row.
pub async fn unaccounted_writers<B: StoreBackend>(
    store: &AccountStore<B>,
    scope: &str,
    stored: &BTreeMap<String, i64>,
) -> Result<BTreeMap<String, i64>> {
    let mut pinned = BTreeMap::new();
    for (writer, seen) in store
        .relay_high_waters(scope, ItemClass::StateEntry.as_wire())
        .await?
    {
        let hex = writer.to_hex();
        let slot = stored.get(&hex).copied().unwrap_or(0);
        if i64::try_from(seen).is_ok_and(|seen| seen <= slot) {
            continue;
        }
        if store.writer_relation(&writer).await?.is_own() {
            continue;
        }
        pinned.insert(hex, slot);
    }
    Ok(pinned)
}

/// Put one last-writer-wins row through the plane's door — the
/// `device_endpoints_writer`'s publish.
pub async fn put_lww_row<B: StoreBackend, R: RpcRequester + Clone>(
    fleet: &AccountStatePlane<'_, B, R>,
    kind: &str,
    key: &str,
    value: Vec<u8>,
    device_id: [u8; 32],
) -> Result<u64> {
    fleet
        .put(
            &ItemId {
                kind: kind.into(),
                key: key.into(),
            },
            value,
            Some(lww_stamp_now(device_id)?),
        )
        .await
}

/// `put_lww_row`'s local half alone ([`AccountStatePlane::put_local`]):
/// the row is durable and stamped, nothing is sent — the account runtime's
/// publish step ships it. The door puts a user's gesture reaches take this.
pub async fn put_lww_row_local<B: StoreBackend, R: RpcRequester + Clone>(
    fleet: &AccountStatePlane<'_, B, R>,
    kind: &str,
    key: &str,
    value: Vec<u8>,
    device_id: [u8; 32],
) -> Result<u64> {
    fleet
        .put_local(
            &ItemId {
                kind: kind.into(),
                key: key.into(),
            },
            value,
            Some(lww_stamp_now(device_id)?),
        )
        .await
}

/// [`put_lww_row_local`]'s deletion: a stamped tombstone at `(kind, key)`,
/// local only ([`AccountStatePlane::tombstone_local`]) — the stamp is what
/// orders the deletion against a concurrent put of the same row.
pub async fn tombstone_lww_row_local<B: StoreBackend, R: RpcRequester + Clone>(
    fleet: &AccountStatePlane<'_, B, R>,
    kind: &str,
    key: &str,
    device_id: [u8; 32],
) -> Result<u64> {
    fleet
        .tombstone_local(
            &ItemId {
                kind: kind.into(),
                key: key.into(),
            },
            Some(lww_stamp_now(device_id)?),
        )
        .await
}

fn lww_stamp_now(device_id: [u8; 32]) -> Result<Vec<u8>> {
    Ok(LwwStamp {
        at_ms: fauna_core::data::Timestamp::now_millis_or_zero() as i64,
        writer: device_id,
    }
    .encode()?)
}

pub fn entry_to_plaintext(entry: &StateEntry) -> EntryPlaintext {
    EntryPlaintext {
        kind: entry.kind.clone(),
        key: entry.key.clone(),
        merge_meta: entry.merge_meta.clone().map(Into::into),
        value: entry.value.clone().into(),
        tombstone: entry.tombstone,
    }
}

/// The authoring writer's coordinates. Both are required on a state-entry row:
/// every class-2 write carries a device `writer_id`, so `None` here is a nest
/// serving a shape this feed does not have, not a legacy row.
pub fn row_coordinates(change: &SyncChange) -> Result<(WriterId, u64)> {
    let hex_writer = change.origin_writer.as_deref().with_context(|| {
        format!(
            "state-entry row at seq {} carries no origin_writer",
            change.seq
        )
    })?;
    let bytes: [u8; 32] = hex::decode(hex_writer)
        .ok()
        .and_then(|b| b.try_into().ok())
        .with_context(|| format!("state-entry row at seq {}: bad origin_writer", change.seq))?;
    let seq = change.origin_seq.with_context(|| {
        format!(
            "state-entry row at seq {} carries no origin_seq",
            change.seq
        )
    })?;
    let seq = u64::try_from(seq)
        .with_context(|| format!("state-entry row at seq {}: negative origin_seq", change.seq))?;
    Ok((WriterId(bytes), seq))
}

/// The blinded item key, which rides the shipped `path_hash` slot (§ The
/// class-2 entry form — "dropping into the feed's existing opaque 32-byte
/// routing-key slot").
pub fn item_key_of(change: &SyncChange) -> Result<[u8; 32]> {
    hex::decode(&change.path_hash)
        .ok()
        .and_then(|b| b.try_into().ok())
        .with_context(|| {
            format!(
                "state-entry row at seq {}: item key is not 32 hex-encoded bytes",
                change.seq
            )
        })
}

#[cfg(test)]
mod door_tests {
    use super::*;

    /// The per-entry cap is unforgettable only while the store write stays
    /// unreachable without a ticket, and no behavioural test can witness
    /// *that*: a sixth arm calling `put_state` directly would journal a row
    /// nothing sized, and every existing test would stay green — which is
    /// exactly how the walk's carry arm came to carry an unwitnessed third
    /// copy of the door. So the durable witness is a **mechanism** pin, the
    /// shape `fauna_core::secret`'s constant-time delegation uses for the
    /// same reason.
    ///
    /// Together with [`AccountStatePlane::put_own_row`]'s signature this is
    /// the whole guarantee: rustc refuses a write without a
    /// [`SizedEntry`], and this refuses a write that goes around
    /// `put_own_row`.
    #[test]
    fn the_plane_has_one_sized_write_onto_our_own_log() {
        let src = include_str!("account_state_plane.rs");
        // Production half only: this module's own source mentions the call.
        let production = src
            .split_once("#[cfg(test)]\nmod door_tests")
            .expect("the test module moved")
            .0;
        let calls: Vec<_> = production.match_indices(".put_state(").collect();
        assert_eq!(
            calls.len(),
            1,
            "the plane grew a second `put_state` call: every row this replica \
             authors must go through `put_own_row`, which is what makes the \
             per-entry cap impossible for a new arm to forget"
        );
        let seam = production
            .split_once("async fn put_own_row")
            .expect("put_own_row went missing")
            .1;
        let seam = &seam[..seam.find("\n    }").expect("unterminated fn")];
        assert!(
            seam.contains(".put_state("),
            "the one `put_state` call left the `put_own_row` seam"
        );
    }
    use crate::generation_fixture_test_support::{ESCROW_SEED, US, device_key, fixture, member_of};
    use fauna_core::generation::{
        EscrowTargetRecord, GenerationMintRecord, MemberWrap, derive_escrow_xwing_keypair,
    };
    use fauna_mls::wrapped_blob::generation_wraps::{build_mint, seal_generation_key_to_device};
    use fauna_protocol::merge_policy::KIND_GENERATION_MINT;

    /// A mint row over `n` members — the minter (`US`) plus `n - 1` others —
    /// with **one inline wrap per member** — the unbounded shape, whose size
    /// grows with the fleet until the per-entry cap refuses it. `build_mint`
    /// bounds the inline set, so the spill's wraps are sealed and appended
    /// here explicitly.
    fn mint_item(n: u8) -> (ItemId, Vec<u8>) {
        let mut members = vec![member_of(US)];
        members.extend((1..n).map(|i| member_of([0x80 + i; 32])));
        members.sort_by_key(|m| m.device_id);
        let escrow = EscrowTargetRecord {
            xwing_escrow_pubkey: derive_escrow_xwing_keypair(&ESCROW_SEED)
                .public
                .to_bytes()
                .to_vec(),
        };
        let built = build_mint(
            &members,
            &escrow,
            &crate::generation_fixture_test_support::target_key(),
            Vec::new(),
            &device_key(US),
            7_000,
        )
        .expect("mint");
        let GenerationMintRecord::Minted {
            core,
            minter_sig,
            mut wraps,
        } = built.record
        else {
            unreachable!("a fresh mint is Minted")
        };
        for m in &built.spilled {
            wraps.push(MemberWrap {
                device_id: m.device_id,
                wrap: seal_generation_key_to_device(
                    &built.gen_key,
                    &m.xwing_pubkey,
                    &built.generation_id,
                    &m.device_id,
                )
                .unwrap(),
            });
        }
        (
            ItemId {
                kind: KIND_GENERATION_MINT.into(),
                key: fauna_core::hex32::encode(&built.generation_id),
            },
            fauna_core::encoding::canonical_encode(&GenerationMintRecord::Minted {
                core,
                minter_sig,
                wraps,
            })
            .unwrap(),
        )
    }

    /// The writer door refuses an entry no nest will accept BEFORE the local
    /// write. `publish_pending` replays rows a network failure left local; a
    /// row over `MAX_STATE_ENTRY_BYTES` would instead be re-sent and refused
    /// on every pass, and since that pass stops at its first failure, every
    /// later row this writer puts in the scope would stay local-only behind it.
    #[tokio::test]
    async fn an_entry_no_nest_accepts_is_refused_before_the_local_write() {
        let fx = fixture().await;
        let plane = fx.plane();

        let (big, big_value) = mint_item(64);
        let err = plane
            .put(&big, big_value, None)
            .await
            .expect_err("a 64-member mint row is over the per-entry cap");
        let msg = format!("{err:#}");
        assert!(msg.contains("per-entry cap"), "{msg}");
        assert!(
            fx.store.state(&big.kind, &big.key).await.unwrap().is_none(),
            "the refusal must leave no local row behind: {msg}"
        );

        // The door still admits what fits.
        let (small, small_value) = mint_item(2);
        plane
            .put(&small, small_value, None)
            .await
            .expect("a 2-member mint row fits");
        assert!(
            fx.store
                .state(&small.kind, &small.key)
                .await
                .unwrap()
                .is_some()
        );
    }

    /// A nest whose state put answers `replaced` as configured and keeps
    /// every request it was sent.
    struct PutDoor {
        replaced: Option<u32>,
        sent: std::sync::Mutex<Vec<AccountStatePutRequest>>,
    }

    impl RpcRequester for PutDoor {
        type Error = Unreachable;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> std::result::Result<Reply, Unreachable>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            use fauna_protocol::{decode_strict, encode_canonical};
            if kind != KIND_STATE_PUT {
                return Err(Unreachable(kind));
            }
            let req: AccountStatePutRequest =
                decode_strict(&encode_canonical(&payload).unwrap()).unwrap();
            let mut sent = self.sent.lock().unwrap();
            sent.push(req);
            let reply = encode_canonical(&AccountStatePutReply {
                seq: sent.len() as i64,
                replaced: self.replaced,
                ..Default::default()
            });
            Ok(decode_strict(&reply.unwrap()).unwrap())
        }
    }

    /// Part (2)'s device half (`delegable-scope-reclamation.md`
    /// § Delegable-scope reclamation, part (2)): the plane sends the
    /// coordinates of the rows it is handed, and forgets its relay copy of
    /// each once the put lands — whether the nest superseded the row or found
    /// it not live. A list on the fleet scope is refused before anything is
    /// written.
    #[tokio::test]
    async fn a_named_relay_copy_is_forgotten_once_the_put_lands() {
        use fauna_protocol::account_state::ACCOUNT_STATE_SCOPE;
        use fauna_protocol::merge_policy::{KIND_MODERATION, MODERATION_KEY};

        let other = WriterId([0x77; 32]);
        let item = ItemId {
            kind: KIND_MODERATION.into(),
            key: MODERATION_KEY.into(),
        };
        let stamp = |at_ms| {
            Some(
                LwwStamp {
                    at_ms,
                    writer: device_key(US).verifying_key().to_bytes(),
                }
                .encode()
                .unwrap(),
            )
        };
        let value =
            fauna_core::encoding::canonical_encode(&fauna_core::data::ModerationConfig::default())
                .unwrap();

        for replaced in [Some(1), Some(0)] {
            let fx = fixture().await;
            let door = PutDoor {
                replaced,
                sent: Default::default(),
            };
            let plane = AccountStatePlane::new(
                &fx.store,
                &door,
                &fx.schedule,
                &fx.writer_key,
                &fx.trust,
                ACCOUNT_STATE_SCOPE,
            )
            .expect("plane");
            let item_key = plane
                .gen0_item_key(&item.kind, &item.key)
                .expect("moderation is gen-0");
            let theirs = RelayRow {
                scope: ACCOUNT_STATE_SCOPE.into(),
                item_class: ItemClass::StateEntry.as_wire().into(),
                writer: other,
                writer_seq: 3,
                item_key: item_key.to_vec(),
                op: OP_STATE_PUT.into(),
                entry: Some(vec![1, 2, 3]),
                feed_seq: Some(9),
            };
            fx.store.record_relay_row(&theirs).await.unwrap();

            plane
                .put_replacing(
                    &item,
                    value.clone(),
                    stamp(100),
                    std::slice::from_ref(&theirs),
                )
                .await
                .expect("the put lands");

            let sent = door.sent.lock().unwrap().clone();
            assert_eq!(sent.len(), 1);
            assert_eq!(
                sent[0].replaces,
                vec![ReplacedRow {
                    item_key: item_key.to_vec().into(),
                    writer_id: other.to_hex(),
                    writer_seq: 3,
                    extra: Default::default(),
                }]
            );
            let held: Vec<_> = plane
                .relay_rows_at(&item_key)
                .await
                .unwrap()
                .into_iter()
                .filter(|r| r.writer == other)
                .collect();
            assert!(
                held.is_empty(),
                "replaced = {replaced:?}: the named copy is forgotten once the put lands"
            );
        }

        // The fleet scope takes no list: refused before the local write.
        let fx = fixture().await;
        let door = PutDoor {
            replaced: Some(1),
            sent: Default::default(),
        };
        let plane = AccountStatePlane::new(
            &fx.store,
            &door,
            &fx.schedule,
            &fx.writer_key,
            &fx.trust,
            ACCOUNT_STATE_FLEET_SCOPE,
        )
        .expect("plane");
        let theirs = RelayRow {
            scope: ACCOUNT_STATE_FLEET_SCOPE.into(),
            item_class: ItemClass::StateEntry.as_wire().into(),
            writer: other,
            writer_seq: 3,
            item_key: vec![0x11; 32],
            op: OP_STATE_PUT.into(),
            entry: Some(vec![1]),
            feed_seq: None,
        };
        let err = plane
            .put_replacing(&item, value, stamp(100), &[theirs])
            .await
            .expect_err("the fleet scope takes no list");
        assert!(format!("{err:#}").contains("fleet scope"), "{err:#}");
        assert!(door.sent.lock().unwrap().is_empty());
        assert!(
            fx.store
                .state(&item.kind, &item.key)
                .await
                .unwrap()
                .is_none()
        );
    }

    fn holder_key() -> SigningKey {
        SigningKey::from_bytes(&[0x66u8; 32])
    }

    #[derive(Debug)]
    struct Unreachable(&'static str);

    impl std::fmt::Display for Unreachable {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "unreachable: {}", self.0)
        }
    }

    impl fauna_protocol::RpcErrorClass for Unreachable {
        fn is_rejection(&self) -> bool {
            false
        }
    }

    /// A session whose escrow door answers (receipts signed by
    /// [`holder_key`]) while its state put is down until `publishes` is set
    /// — the holder reachable, the publish not.
    #[derive(Default)]
    struct EscrowDoorOnly {
        publishes: std::sync::atomic::AtomicBool,
        deposits: std::sync::atomic::AtomicUsize,
        /// The writer seq of every row the state put accepted, in order.
        sent: std::sync::Mutex<Vec<i64>>,
    }

    impl RpcRequester for EscrowDoorOnly {
        type Error = Unreachable;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> std::result::Result<Reply, Unreachable>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            use fauna_protocol::generation_escrow::{
                EscrowPutReply, EscrowPutRequest, KIND_ESCROW_PUT,
            };
            use fauna_protocol::{decode_strict, encode_canonical};
            use std::sync::atomic::Ordering;

            let payload = encode_canonical(&payload).unwrap();
            let reply = match kind {
                KIND_ESCROW_PUT => {
                    let req: EscrowPutRequest = decode_strict(&payload).unwrap();
                    self.deposits.fetch_add(1, Ordering::SeqCst);
                    let receipt = fauna_core::generation::sign_escrow_receipt(
                        &holder_key(),
                        req.generation_id.as_slice().try_into().unwrap(),
                        blake3::hash(&req.wrap).into(),
                        &req.target_key,
                        7_000,
                    );
                    encode_canonical(&EscrowPutReply {
                        receipt: fauna_core::encoding::canonical_encode(&receipt)
                            .unwrap()
                            .to_vec()
                            .into(),
                        extra: Default::default(),
                    })
                }
                KIND_STATE_PUT if self.publishes.load(Ordering::SeqCst) => {
                    let req: AccountStatePutRequest = decode_strict(&payload).unwrap();
                    let mut sent = self.sent.lock().unwrap();
                    sent.push(req.writer_seq);
                    encode_canonical(&AccountStatePutReply {
                        seq: sent.len() as i64,
                        ..Default::default()
                    })
                }
                _ => return Err(Unreachable(kind)),
            };
            Ok(decode_strict(&reply.unwrap()).unwrap())
        }
    }

    /// **A first-need mint whose holder answered stands, whether or not its
    /// rows can be sent** (`account-data-taxonomy.md` § The generation
    /// machinery → *The mint protocol*, trigger (a): the mint needs an escrow
    /// target and a reachable holder, nothing else). The deposit is the
    /// mint's one step that must be online; its rows are local-first like
    /// every other write, so a state put that fails right after the deposit
    /// must not abort the sequence between the mint row and the receipt row.
    /// That abort refused the write that tripped the mint, left an unacked
    /// mint row behind, and spent a fresh deposit on every retry.
    #[tokio::test]
    async fn a_first_need_mint_stands_when_its_rows_cannot_be_sent_yet() {
        use fauna_core::group_generation::GroupReceptionKeyRecord;
        use std::sync::atomic::Ordering;

        let mut fx = fixture().await;
        fx.trust.trusted_holders = vec![holder_key().verifying_key().to_bytes()].into();
        fx.put(crate::generation_fixture_test_support::enrollment_row(US))
            .await;
        fx.put(
            crate::generation_mint::escrow_target_entry(
                &crate::generation_fixture_test_support::ROOT_SEED,
            )
            .unwrap(),
        )
        .await;
        let door = EscrowDoorOnly::default();
        let plane = AccountStatePlane::new(
            &fx.store,
            &door,
            &fx.schedule,
            &fx.writer_key,
            &fx.trust,
            fauna_protocol::account_state::ACCOUNT_STATE_FLEET_SCOPE,
        )
        .expect("plane");

        let first = GroupReceptionKeyRecord::mint(1);
        crate::group_state_plane::write_reception_key_row(&plane, &first)
            .await
            .expect("the holder answered, so the mint stands and the tip-sealed row lands");
        assert_eq!(door.deposits.load(Ordering::SeqCst), 1);
        assert!(
            generation_tip::resolve_tip(&fx.store, &fx.trust, &fx.writer_key, None)
                .await
                .unwrap()
                .tip
                .is_some(),
            "the receipt row is durable beside the mint row, so the tip resolves here"
        );

        // The next tip-sealed write finds that tip: no second generation.
        let second = GroupReceptionKeyRecord::mint(2);
        crate::group_state_plane::write_reception_key_row(&plane, &second)
            .await
            .expect("a second tip-sealed row seals under the same tip");
        assert_eq!(
            door.deposits.load(Ordering::SeqCst),
            1,
            "one generation, however many writes waited on the publish"
        );
        assert!(door.sent.lock().unwrap().is_empty());

        // The session's state put comes back: the mint's rows and the rows
        // sealed under them go out in journal order — a reader holding a
        // tip-sealed row already holds the mint and its receipt.
        door.publishes.store(true, Ordering::SeqCst);
        let published = plane.publish_pending().await.expect("the publish lands");
        let sent = door.sent.lock().unwrap().clone();
        assert_eq!(sent.len(), published);
        assert!(
            sent.len() >= 4,
            "the mint row, its receipt and both tip-sealed rows: {sent:?}"
        );
        assert!(
            sent.windows(2).all(|w| w[0] < w[1]),
            "journal order: {sent:?}"
        );
    }
}
