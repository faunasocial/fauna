//! The trait-abstracted physical backend — "the store API is the seam"
//! (charter: `account-data-plane.md` § Store logical schema, Physical
//! realization).
//!
//! A backend is *dumb and atomic*: every method is one transaction over the
//! physical medium, and every invariant that makes the store a store —
//! gapless local append, verbatim idempotent ingest with equivocation
//! refusal, accounted-only frontier advance — lives ONCE in
//! [`crate::store::AccountStore`], above this trait, so web's parallel
//! backend (IndexedDB/OPFS via WASM, W6 (account-data-plane.md § Workstreams)) inherits them by construction.
//!
//! Methods are `async fn` (AFIT — the `RpcRequester` precedent): the native
//! SQLite backend resolves immediately from synchronous bodies, while
//! IndexedDB is async-only, and an async seam can wrap a sync medium where a
//! sync seam could never wrap an async one. This trait must compile for
//! `wasm32-unknown-unknown`; nothing in it may name a native-only type.

use anyhow::Result;
use fauna_core::data::ContentHash;

use crate::segments::{AdoptedBlock, SegmentHalf, SegmentKey, SegmentSink};
use crate::types::{
    InsertOutcome, IssuedRetire, ItemRef, JournalRow, NewOutboxIntent, OutboxIntent,
    RecordIndexEntry, RelayRow, StateEntry, WriterId,
};

/// A readable, seekable staged `.dat` — what [`crate::segments::admit`] reads.
pub trait ReadSeek: std::io::Read + std::io::Seek {}
impl<T: std::io::Read + std::io::Seek> ReadSeek for T {}

/// One incoming segment pair, staged in the backend's own segment area
/// ([`StoreBackend::segment_stage`]) — written chunk by chunk as a
/// [`SegmentSink`], verified by reading it back, then either adopted by
/// [`StoreBackend::segment_adopt`] (a rename, never a copy) or dropped.
///
/// **Dropping a slot un-adopted discards its bytes.** A slot a crash leaves
/// behind is an unrouted staging file no reader reaches — never a row naming a
/// missing file — and the backend sweeps it at its next open, the way it
/// replays a scope drop's pending file sweep.
#[allow(async_fn_in_trait)]
pub trait SegmentStaging: SegmentSink {
    /// Bytes staged so far for `half`.
    fn len(&self, half: SegmentHalf) -> u64;

    /// The staged `.meta`, whole — the admission's own index, as small as the
    /// sidecar is (it grows with the record count, not the record bytes).
    async fn meta(&mut self) -> Result<Vec<u8>>;

    /// The staged `.dat`, read back from where it rests (buffered by
    /// [`crate::segments::SEGMENT_TRANSFER_CHUNK`]).
    async fn dat_reader(&mut self) -> Result<Box<dyn ReadSeek + '_>>;

    /// A crash's leftover, for the conformance suite: close the slot as a
    /// dying process would — bytes on disk, no lock held, nothing adopted, no
    /// cleanup run.
    #[cfg(any(test, feature = "test-helpers"))]
    async fn abandon_as_crash(self);
}

#[allow(async_fn_in_trait)] // see RpcRequester: static dispatch only; per-impl Send
pub trait StoreBackend {
    /// This backend's staging slot ([`Self::segment_stage`]).
    type Staging: SegmentStaging;

    /// The medium's cross-connection change counter — the W5.2 notification
    /// floor (charter § Multi-instance concurrency, T9 *poll-with-poke*): a
    /// value that moves when **another connection** committed to the same
    /// store since this backend last read it, and never from this backend's
    /// own writes. SQLite answers with `PRAGMA data_version`; a backend whose
    /// medium has no such counter answers `None` (its platform's poke — web's
    /// BroadcastChannel — is then the only notice, exactly the fast-path-only
    /// posture the charter assigns it). `None` therefore means "no counter",
    /// never "no change".
    async fn data_version(&self) -> Result<Option<u64>> {
        Ok(None)
    }

    /// Read one store-meta value (`store_meta` is the KV plane of charter
    /// § Store logical schema component 4).
    async fn meta_get(&self, key: &str) -> Result<Option<Vec<u8>>>;

    /// Read **several store-meta values in ONE transaction** — one consistent
    /// snapshot, in the order given, `None` for an absent key. The read
    /// sibling of [`Self::meta_put_all`], and for the same reason: the
    /// succession fence writes the writer stamp and the pending-re-author
    /// marker together, so a reader that takes them in two transactions can
    /// see a pre-fence stamp beside a post-fence marker — and the re-author
    /// pass, filtering the marker against the stamp, then reads its own
    /// predecessor as hand damage and clears it (charter § The store device
    /// principal → succession decision 3; the loss the marker exists to
    /// prevent).
    ///
    /// **Implementors:** one transaction, no exceptions — a snapshot
    /// assembled from N separate reads is exactly the bug this exists to
    /// close, and it would be invisible in every single-threaded test.
    async fn meta_get_all(&self, keys: &[&str]) -> Result<Vec<Option<Vec<u8>>>>;

    /// Write one store-meta value (upsert, atomic).
    async fn meta_put(&self, key: &str, value: &[u8]) -> Result<()>;

    /// Write **several store-meta values in ONE transaction** — all land or
    /// none. Plain upserts (the byte-value sibling of
    /// [`Self::meta_put_pair_max`], without the numeric rising-only guard):
    /// the consumer is the succession fence, whose writer re-stamp,
    /// pending-re-author marker and retired-writers memory must never be
    /// observable half-written — a fence without its marker strands the
    /// un-pushed tail silently, and one without the retiree makes the walk
    /// false-equivocate against the machine's own former rows.
    async fn meta_put_all(&self, pairs: &[(&str, &[u8])]) -> Result<()>;

    /// Delete one store-meta value. Absent is fine (idempotent).
    async fn meta_delete(&self, key: &str) -> Result<()>;

    /// **Compare-and-delete, in ONE transaction:** delete every named key,
    /// but only if *all* of them still hold exactly the value the caller
    /// read — `Some(bytes)` for a value it saw, `None` for a key it saw
    /// absent. Reports whether the delete happened; a mismatch changes
    /// nothing and is a benign verdict, never an error.
    ///
    /// A consistent read ([`Self::meta_get_all`]) is not enough on its own
    /// for a probe-then-delete: the decision to clear is taken on a snapshot,
    /// and the fence can land in the gap between deciding and deleting — so
    /// the delete must re-assert the picture it was decided on. The consumer
    /// is the re-author marker clear (`clear_writer_reauthor_if_unchanged`),
    /// where deleting against a stale picture strands a predecessor's
    /// un-pushed tail with nobody left to re-author it.
    ///
    /// **Implementors:** the compare and the delete share one transaction, no
    /// exceptions — split across two, this is the very race it exists to
    /// close.
    async fn meta_delete_all_if_unchanged(
        &self,
        expected: &[(&str, Option<&[u8]>)],
    ) -> Result<bool>;

    /// Write **two numeric store-meta values, rising-only, in ONE
    /// transaction** — either both writes land or neither, and each key's
    /// stored value only ever *rises*: a write that would lower a value
    /// leaves that key unchanged (per key, independently). Values rest in
    /// the same ASCII-decimal encoding `meta_put` callers use.
    ///
    /// One transaction because the store's at-rest format pair
    /// (`format_version` + `min_reader_format_version`) may be read by a
    /// concurrent cold opener at any moment, and
    /// [`AccountStore::open`](crate::store::AccountStore::open) *refuses to
    /// guess* at half a pair — two separate transactions give that opener a
    /// window in which the store legitimately reads as corrupt (W5.3,
    /// charter § Multi-instance concurrency).
    ///
    /// Rising-only because the stamp races processes the migration lock
    /// cannot serialize — adoption runs after the backend's own open has
    /// released the lock, and a `Degraded` lock proceeds unserialized by
    /// design — so monotonicity must live in the write itself: the nest
    /// `record_schema_meta` guarded-upsert idiom (`version-compatibility.md`
    /// § 2.2, "do not restamp down"). Per-key max makes the pair a lattice
    /// join: racing version-skewed binaries converge to the honest union in
    /// any order.
    ///
    /// **Implementors:** one transaction + the rising-only guard, no
    /// exceptions — a backend that cannot do both cannot host concurrent
    /// instances, and should say so rather than approximate.
    async fn meta_put_pair_max(&self, first: (&str, u16), second: (&str, u16)) -> Result<()>;

    /// Insert one journal row keyed on `(writer, seq)`, atomically. Rows are
    /// immutable: an occupied slot is never overwritten — the outcome reports
    /// whether the occupant is byte-identical.
    ///
    /// `local_writer` is the **append-time writer guard** (charter § The
    /// store device principal → *Principal succession*, decision 4): when
    /// `Some(w)`, the backend must verify — inside the SAME transaction as
    /// the insert — that the store's writer-identity meta still equals `w`,
    /// refusing typed ([`crate::store::StaleWriter`]) on mismatch. `None` is
    /// an ingest of another writer's row, never guarded. An absent identity
    /// meta passes: the store is stamped at open, before any local append,
    /// so the case is unreachable in production and meaningless to refuse.
    async fn insert_row(
        &self,
        row: &JournalRow,
        local_writer: Option<&WriterId>,
    ) -> Result<InsertOutcome>;

    /// Atomically upsert `entry` AND insert its `state-put` journal `row` in
    /// one transaction. On any non-`Inserted` outcome, *neither* write
    /// happens — the entry upsert must never land without its journal row.
    /// `local_writer`: the append-time writer guard, as on
    /// [`Self::insert_row`].
    ///
    /// **The entry's version is verified in the same transaction, before
    /// anything is written:** `entry.entry_version` must be the stored
    /// entry's version plus one (1 when none is stored), else the answer is
    /// [`InsertOutcome::EntryMoved`]. The caller reads the version outside
    /// this transaction, and several same-account instances write one store
    /// (`account-runtime.md` § Multi-instance concurrency → *Store
    /// contract*): without the check two of them land the same version of
    /// one entry under two journal rows, and a version stops naming one
    /// value — which the walk reads as a journal that is not this store's
    /// own, and rotates the writer away.
    async fn state_put_with_row(
        &self,
        entry: &StateEntry,
        row: &JournalRow,
        local_writer: Option<&WriterId>,
    ) -> Result<InsertOutcome>;

    /// Read the current entry for `(kind, key)`.
    async fn state_get(&self, kind: &str, key: &str) -> Result<Option<StateEntry>>;

    // ── The group plane's entry table ─────────────────────────────────────────
    //
    // The group siblings of `state_put_with_row`/`state_get`, keyed
    // `(scope, kind, key)` because one member holds MANY group scopes and
    // every scope carries rows at the same logical keys (each scope has a
    // `fauna.group.birth` row at `"self"`). Journal rows, frontiers, and
    // relay rows for group scopes ride the existing scope-keyed methods —
    // only the merged-current-value table is plane-specific.

    /// Atomically upsert the group-plane `entry` AND insert its journal `row`
    /// in one transaction — [`Self::state_put_with_row`]'s contract on the
    /// `group_entries` table, the version check included, `entry.scope`
    /// naming the group scope.
    async fn group_state_put_with_row(
        &self,
        entry: &StateEntry,
        row: &JournalRow,
        local_writer: Option<&WriterId>,
    ) -> Result<InsertOutcome>;

    /// Read the current group-plane entry for `(scope, kind, key)`.
    async fn group_state_get(
        &self,
        scope: &str,
        kind: &str,
        key: &str,
    ) -> Result<Option<StateEntry>>;

    /// Every current group-plane entry in `scope`, ordered `(kind, key)` for
    /// deterministic reads — the listing read the app surfaces (and the
    /// resolver walks) build on.
    async fn group_states_for_scope(&self, scope: &str) -> Result<Vec<StateEntry>>;

    /// The highest `seq` this store holds on `writer`'s log, across scopes.
    async fn max_writer_seq(&self, writer: &WriterId) -> Result<Option<u64>>;

    /// The highest `seq` this store holds on `writer`'s log *within* `scope` —
    /// the accounting bound for a frontier advance.
    async fn max_scope_writer_seq(&self, scope: &str, writer: &WriterId) -> Result<Option<u64>>;

    /// `writer`'s rows in `scope` with `seq > after`, ascending, at most
    /// `limit`.
    async fn rows_for_scope(
        &self,
        scope: &str,
        writer: &WriterId,
        after: u64,
        limit: u32,
    ) -> Result<Vec<JournalRow>>;

    /// The scope-feed coordinate this replica journaled for `item` in `scope`
    /// — the `(writer, seq)` pair a seen-set itemizes
    /// ([`fauna_core::seen_set::SeenRef`]).
    ///
    /// The **lowest** `seq` carrying the item wins, so the coordinate is the
    /// row that introduced the record and stays stable as later ops (a
    /// removal, a re-add) land on the same item. `None` when this scope's
    /// journal holds no row for the item — the T1 intake's
    /// *unresolvable-observation* case, which it drops and re-records on a
    /// later render rather than keeping pending state
    /// (`account-data-plane.md` § The replica boundary → T1).
    ///
    /// Deterministic across replicas: the coordinates are the nest
    /// sequencer's, so two devices resolve the same record to the same pair.
    async fn coordinate_of_item(
        &self,
        scope: &str,
        item: &ItemRef,
    ) -> Result<Option<(WriterId, u64)>>;

    /// The stored frontier vector for `scope`: one `(writer, high_seq)` pair
    /// per writer, unordered.
    async fn frontier(&self, scope: &str) -> Result<Vec<(WriterId, u64)>>;

    /// Raise `writer`'s high-water in `scope` to `max(current, seq)`,
    /// atomically, returning the resulting high-water. The MAX-merge makes
    /// regression *unrepresentable* at the physical layer (charter § The
    /// frontier vector: "it never regresses").
    async fn frontier_raise(&self, scope: &str, writer: &WriterId, seq: u64) -> Result<u64>;

    /// `scope`'s **serve-order watermark** (charter § Feeds and cursors →
    /// *Compaction is a serve-order watermark*): the nest-log `seq` through
    /// which this store holds every row the scope's sequencing nest serves
    /// from a writer a request does not name. `None` until a nest's echo has
    /// been banked. One per scope, because a scope has one sequencing nest.
    async fn nest_watermark(&self, scope: &str) -> Result<Option<u64>>;

    /// Raise `scope`'s watermark to `max(current, seq)`, atomically, returning
    /// the result — rising-only for the frontier's reason: a banked watermark
    /// asserts a held prefix of the nest's log, and a racing pass must
    /// max-merge, never clobber. [`Self::drop_scope`] clears it together with
    /// the rows it described.
    async fn nest_watermark_raise(&self, scope: &str, seq: u64) -> Result<u64>;

    /// Clear `scope`'s watermark outright — the ONE way it may ever move down
    /// (to absent), because a raise's max-merge cannot express "forget this".
    /// The caller's law: only after a watermark-bearing request came back with
    /// no echo, meaning the nest that banked it no longer honours the watermark
    /// (rolled back to a pre-watermark image) — never on an ordinary walk. Left
    /// in place, the next walk would reopen narrow against the same
    /// non-honouring nest and repeat the same full re-serve forever
    /// (account-sync-plane.md § Feeds and cursors). [`Self::drop_scope`] also
    /// clears it, together with the rows it described.
    async fn nest_watermark_clear(&self, scope: &str) -> Result<()>;

    // ── The relay plane (W2.6, charter § The peer leg) ────────────────────────
    //
    // One live wire row per `(scope, writer, item)` — what this replica can
    // serve a peer verbatim. Additive at-rest: a build predating it simply
    // has no relay rows to serve (see `types::RelayRow`).

    /// Record (or supersede) the live relay row at
    /// `(row.scope, row.writer, row.item_key)`. A row with `writer_seq` at or
    /// below the held one is a no-op — replays and out-of-order pages must
    /// never regress the live row.
    async fn relay_put(&self, row: &RelayRow) -> Result<()>;

    /// Delete the relay rows held at `(scope, writer, writer_seq)` under any
    /// item OTHER than `keep_item_key`, returning how many went. The walk's
    /// carry arm for a retired burnt writer
    /// (`account-replica-posture.md` § The store device principal,
    /// refinement 11): the fleet serves the coordinate under `keep_item_key`,
    /// so whatever this replica recorded there under another item — a row
    /// the nest refused — is not this replica's word to relay. Idempotent.
    async fn relay_retire_shadowed(
        &self,
        scope: &str,
        writer: &WriterId,
        writer_seq: u64,
        keep_item_key: &[u8],
    ) -> Result<u64>;

    /// Delete every relay row held at `(scope, writer, writer_seq)`, returning
    /// how many went — the nest's **final refusal** of that coordinate
    /// (`account-replica-posture.md` § The store device principal,
    /// refinement 11 → *a refused row's relay residue*): a row the nest
    /// answered `stale_writer_seq` to, or one `publish_pending` skips over the
    /// entry cap, is a row no nest will ever serve, so this replica must not
    /// serve it as its own word either. The table's key is `(scope, writer,
    /// item)`, so this drops the writer's ONLY relay row for the item: the
    /// walk's own echo re-records a row the nest turns out to hold (a
    /// replay), and a burnt row's value re-authors under the successor.
    /// Idempotent.
    async fn relay_retire_at(&self, scope: &str, writer: &WriterId, writer_seq: u64)
    -> Result<u64>;

    /// Live relay rows in `scope` of `item_class` **past** `frontier` (a
    /// writer absent from it is at high-water 0), ordered `(writer,
    /// writer_seq)` ascending, at most `limit` — ordered prefixes per writer,
    /// the feed contract's paging shape (charter § Feeds and cursors).
    async fn relay_rows(
        &self,
        scope: &str,
        item_class: &str,
        frontier: &[(WriterId, u64)],
        limit: u32,
    ) -> Result<Vec<RelayRow>>;

    /// Forget the merged state entry at `(kind, key)` **locally** — no journal
    /// row, no tombstone, nothing published: local hygiene for a row the
    /// reclamation pass knows is dead everywhere (a top-up cell for a
    /// shredded generation or a covered target, a removed device's reach).
    /// A row the feed still serves simply re-adopts on the next reconcile.
    /// A no-op for an absent entry.
    async fn state_forget(&self, kind: &str, key: &str) -> Result<()>;

    /// Drop the live relay row at `(scope, writer, item_key)` **locally** —
    /// the relay-plane twin of [`Self::state_forget`]: a row its writer has
    /// retired from the feed is nothing a peer should be served, and
    /// nothing a later pass should keep naming. A no-op for an absent row.
    async fn relay_forget(&self, scope: &str, writer: &WriterId, item_key: &[u8]) -> Result<()>;

    /// The distinct generations the cleartext headers of `scope`'s live relay
    /// rows name (form v2 only), unordered — answered from an index, so its
    /// cost follows the generations the plane still holds rows under, never
    /// the rows it ever walked.
    async fn relay_generations(&self, scope: &str) -> Result<Vec<[u8; 32]>>;

    /// Drop, **locally**, every relay row in `scope` whose cleartext header
    /// names `generation`, whoever wrote it; returns how many went. The
    /// reclamation pass's sweep of a shredded generation's residue — rows no
    /// key opens any more (`account-data-taxonomy.md` § The generation
    /// machinery → *Fleet-scope reclamation*, clause (3)(h)).
    async fn relay_forget_sealed_under(&self, scope: &str, generation: &[u8; 32]) -> Result<u64>;

    /// Every live relay row in `scope` whose cleartext header names
    /// `generation`, whoever wrote it, in no promised order — answered from
    /// the same index as [`Self::relay_generations`]. The let-go's read: a
    /// dead generation's rows are named and counted by their coordinates,
    /// never opened (`account-data-taxonomy.md` § The generation machinery →
    /// *Fleet-scope reclamation*, clause (3)(j)).
    async fn relay_rows_sealed_under(
        &self,
        scope: &str,
        generation: &[u8; 32],
    ) -> Result<Vec<RelayRow>>;

    /// Every live relay row in `scope` of `item_class` at one blinded
    /// `item_key`, one per writer — the reclamation pass's "which writers hold
    /// a live row for this item, at which coordinate" question, answered by
    /// index rather than by paging the whole plane (which grows with every
    /// row ever walked).
    async fn relay_rows_at(
        &self,
        scope: &str,
        item_class: &str,
        item_key: &[u8],
    ) -> Result<Vec<RelayRow>>;

    /// Every live relay row in `scope` of `item_class` authored by `writer`,
    /// ascending `writer_seq` — the reclamation pass's view of one removed
    /// writer's rows.
    async fn relay_rows_of_writer(
        &self,
        scope: &str,
        item_class: &str,
        writer: &WriterId,
    ) -> Result<Vec<RelayRow>>;

    /// Stamp the live relay row at exactly `(scope, writer, item_key,
    /// writer_seq)` with its feed coordinate ([`RelayRow::feed_seq`]) — the
    /// publish leg's record of the `seq` the nest's put reply assigned, and
    /// the one column a replay at held coordinates may fill in. A no-op for
    /// an absent row or a row the store has since superseded.
    async fn relay_stamp_feed_seq(
        &self,
        scope: &str,
        writer: &WriterId,
        item_key: &[u8],
        writer_seq: u64,
        feed_seq: u64,
    ) -> Result<()>;

    /// Clear the feed coordinate ([`RelayRow::feed_seq`]) of every relay row
    /// of `scope`, keeping the rows — the coordinates are positions in the
    /// log of a replica whose watermark was just voided
    /// ([`crate::store::AccountStore::void_nest_watermark`]). Idempotent.
    async fn relay_clear_feed_seqs(&self, scope: &str) -> Result<()>;

    /// Record one retire a bound plane sent — the **retire record**
    /// (`account-sync-plane.md` § The bind leg, ruling 5), keyed by the
    /// retire's coordinates `(scope, writer, item_key, writer_seq)`: a retire
    /// asked again at the same coordinates (a deferred one, every pass)
    /// replaces its entry — belt and answer included — and becomes the newest.
    /// The record then keeps its newest `cap` entries and drops the rest, in
    /// the same transaction.
    async fn issued_retire_put(&self, retire: &IssuedRetire, cap: u32) -> Result<()>;

    /// The retire record, oldest first, each entry with its **order token** —
    /// rising with every put, so the newest entry carries the highest.
    async fn issued_retires(&self) -> Result<Vec<(u64, IssuedRetire)>>;

    /// Drop every entry of the retire record whose order token is at or below
    /// `through` — what a reader that read up to `through` clears, leaving an
    /// entry another process recorded since. Idempotent.
    async fn issued_retires_clear_through(&self, through: u64) -> Result<()>;

    /// Per writer, the highest `writer_seq` among `scope`'s live relay rows of
    /// `item_class`, unordered — what this store has **seen** of each writer,
    /// beside the frontier's what it has **accounted**. A foreign writer seen
    /// above its stored slot has a row this store holds verbatim but never
    /// accounted (left unopened or unmergeable) — the writer a nest-leg
    /// request projected under a watermark must keep naming.
    async fn relay_high_waters(
        &self,
        scope: &str,
        item_class: &str,
    ) -> Result<Vec<(WriterId, u64)>>;

    /// Meter the relay plane per `(scope, item_class)`: rows held, payload
    /// bytes at rest, and the **evictable** subset of each — rows carrying
    /// payload whose `op` is not one of `floor_ops`.
    ///
    /// `floor_ops` is supplied by the caller rather than known here on purpose:
    /// this layer never interprets `op` (it is opaque cleartext, R7 (account-data-plane.md § The ratified decisions)), and the
    /// wire strings are `fauna_protocol`'s to own — a constant duplicated down
    /// here would be free to drift from the one the walk writes.
    ///
    /// Feeds `fauna_core::custody_policy::CustodyMeter`; keyless by
    /// construction, since it reads only coordinates and `length(entry)`.
    async fn relay_meter(&self, floor_ops: &[&str]) -> Result<Vec<RelayScopeMeter>>;

    /// Drop payload bytes from `(scope, item_class)` until `target_bytes` have
    /// been freed, **oldest `writer_seq` first**, never touching a row whose
    /// `op` is in `floor_ops` and never touching a row's coordinates.
    ///
    /// This is T15's payload-only eviction: the row survives with `entry` NULL,
    /// so the coordinate floor — what a peer's frontier accounts and what the
    /// served shape is made of — is unchanged, and the walk's existing
    /// entry-less branch handles the result with no new code path. It is
    /// **dehydration, never deletion**, in exactly the sense
    /// [`StoreBackend::block_delete`] means it.
    ///
    /// Returns what was actually freed, which may be less than asked when the
    /// family has less evictable payload than the caller believed (a meter is a
    /// snapshot; a concurrent pull may have superseded rows since).
    async fn relay_evict_payload(
        &self,
        scope: &str,
        item_class: &str,
        target_bytes: u64,
        floor_ops: &[&str],
    ) -> Result<RelayEvicted>;

    /// Every live (non-tombstone) state entry of `kind`, ordered by key — the
    /// per-kind enumeration discovery reads (`fauna.state.device-endpoints`
    /// keeps one entry per device, keyed by its writer id hex).
    async fn state_entries_of_kind(&self, kind: &str) -> Result<Vec<StateEntry>>;

    // ── The block plane (charter § Store logical schema component 1) ─────────
    //
    // Two planes, deliberately separable: the **record index** is always
    // present for every record this replica knows of, while **block bytes**
    // come and go under hydration policy. Nothing here interprets a block —
    // bytes are opaque, which is what keeps a key-less store first-class (R7).
    //
    // Physical *placement* (loose block vs. adopted segment file) is not in
    // this seam on purpose: the charter calls it "a local detail, never an
    // identity", and a store may fold loose blocks into segments as local
    // compaction with nothing observing the difference through the store API.
    // W2.2's segment adoption therefore lands entirely behind these methods.

    /// Store a block's bytes under its CID. Idempotent; callers above have
    /// already verified the CID matches (see
    /// [`crate::store::AccountStore::put_block`]).
    async fn block_put(&self, cid: &ContentHash, bytes: &[u8]) -> Result<()>;

    /// A block's bytes, if this replica currently holds them — **from either
    /// placement**: the loose area, or an adopted segment (W2.2), read back
    /// through that segment's own CARv2 index. Which one served the read is
    /// not observable, by design.
    async fn block_get(&self, cid: &ContentHash) -> Result<Option<Vec<u8>>>;

    /// Whether the bytes are held — the presence question, answered without
    /// reading them, and true for either placement.
    async fn block_has(&self, cid: &ContentHash) -> Result<bool>;

    /// Drop a block's **loose** bytes, keeping its index row. Returns whether
    /// loose bytes were actually held. This is dehydration, never deletion:
    /// the record's identity and index row survive.
    ///
    /// A segment-resident block is untouched — a CARv2 file is immutable, and
    /// reclaiming one is segment eviction, a whole-file operation. The logical
    /// layer refuses that case up front rather than letting this return `true`
    /// for a block the store would still serve
    /// ([`crate::store::AccountStore::dehydrate`]).
    async fn block_delete(&self, cid: &ContentHash) -> Result<bool>;

    /// Upsert one record-index row.
    async fn record_index_put(&self, entry: &RecordIndexEntry) -> Result<()>;

    /// One record-index row by CID.
    async fn record_index_get(&self, cid: &ContentHash) -> Result<Option<RecordIndexEntry>>;

    /// Drop one record-index row. Returns whether a row was held.
    ///
    /// The index is the always-present layer, so removing a row is what makes a
    /// record *gone* to every store API — the class-1 tombstone's effect
    /// ([`crate::store::AccountStore::apply_tombstone`] pairs it with the block
    /// plane). Deliberately not an upsert-to-a-flag: a tombstoned record has no
    /// state left worth describing, and a "deleted" flag would have to be
    /// filtered out of every read path forever.
    async fn record_index_delete(&self, cid: &ContentHash) -> Result<bool>;

    /// A scope's index rows ordered by CID, starting after `after`, at most
    /// `limit`. CID order is arbitrary but *stable*, which is all a resumable
    /// walk needs.
    async fn records_in_scope(
        &self,
        scope: &str,
        after: Option<&ContentHash>,
        limit: u32,
    ) -> Result<Vec<RecordIndexEntry>>;

    /// Atomically stage a locally-authored record: its index row, its block
    /// bytes when supplied, and its `record-added` journal row, in ONE
    /// transaction. On any non-`Inserted` outcome nothing is written.
    ///
    /// The atomicity matters for the same reason `state_put_with_row`'s does:
    /// an index row without its journal row is a record no peer will ever hear
    /// about, and a journal row without its index row announces a record this
    /// replica cannot describe.
    /// `local_writer`: the append-time writer guard, as on
    /// [`Self::insert_row`] (this path is local-authorship-only, so callers
    /// always guard it in production).
    async fn record_added_with_row(
        &self,
        entry: &RecordIndexEntry,
        bytes: Option<&[u8]>,
        row: &JournalRow,
        local_writer: Option<&WriterId>,
    ) -> Result<InsertOutcome>;

    // ── Adopted segments: the block plane's second placement (W2.2) ──────────
    //
    // These are the *bulk* door, not a placement knob: nothing above the store
    // asks whether a block is loose or in a segment, and `block_get` answers
    // from either. What a backend owes is (a) keeping both offered files
    // byte-for-byte — a re-encoded `.dat` is no longer the CARv2 file a
    // third-party tool can read, and the `.meta` is the index-rebuild source —
    // and (b) routing a CID to the segment that holds it.

    /// Open a staging slot for one incoming segment pair, inside this
    /// backend's segment area — so a transfer's bytes go to disk as they
    /// arrive and adoption is a rename, not a copy
    /// (`message-segment-store.md` § Segment size: a segment has no size
    /// ceiling, so no transfer holds a half in memory).
    async fn segment_stage(&self) -> Result<Self::Staging>;

    /// File a verified, staged segment pair verbatim, atomically: both files
    /// (renamed into place from `staged`), the `(cid → segment)` routing rows
    /// for `blocks`, and nothing else. Idempotent — re-adopting the same key
    /// is a no-op reporting `false` (the slot is discarded).
    ///
    /// Files first, rows second: a crash between leaves an unrouted file no
    /// reader reaches, never a routing row naming a missing file.
    ///
    /// The pair MUST have come through [`crate::segments::admit`]; a backend
    /// performs no verification of its own (it cannot — verification is a
    /// logical-layer invariant, written once, per this trait's contract).
    async fn segment_adopt(
        &self,
        key: &SegmentKey,
        staged: Self::Staging,
        blocks: &[AdoptedBlock],
    ) -> Result<bool>;

    /// The segments this replica holds for `scope`, ascending by
    /// `(kind, segment_id)`.
    async fn segments_in_scope(&self, scope: &str) -> Result<Vec<SegmentKey>>;

    /// One adopted segment's verbatim `.meta` sidecar bytes — what an index
    /// rebuild reads `record_order` out of.
    async fn segment_meta(&self, key: &SegmentKey) -> Result<Option<Vec<u8>>>;

    /// Which adopted segment holds `cid`, if any. The routing question, asked
    /// without reading bytes.
    async fn segment_of_block(&self, cid: &ContentHash) -> Result<Option<SegmentKey>>;

    /// A block's payload length as the adopting segment recorded it, without
    /// reading the block.
    async fn segment_block_len(&self, cid: &ContentHash) -> Result<Option<u64>>;

    /// The adopted-segment plane, metered per scope — the second half of the
    /// custody meter beside [`Self::relay_meter`] (T15 bounds *retained bytes*,
    /// and a custodian retains both planes). One entry per scope holding any
    /// segment row, evicted ones included. Ascending by scope.
    ///
    /// Read from the rows' recorded lengths, never by re-reading the files:
    /// adoption records the `.dat` length it wrote, and eviction clears it in
    /// the same transaction that drops the segment's routing rows.
    async fn segment_meter(&self) -> Result<Vec<SegmentScopeMeter>>;

    /// Evict whole adopted `.dat` files from `scope`, **oldest segment first**
    /// (ascending `segment_id`, then `kind`), until at least `target_bytes`
    /// have been freed — overshooting by at most one file, because a CARv2
    /// container is immutable and the whole file is the eviction unit
    /// (`message-segment-store.md` § Nest dehydration).
    ///
    /// What survives is the metadata floor: the `segments` row and its `.meta`
    /// sidecar, so re-offering the segment is still "already held" and never a
    /// re-download, and the scope's `record_index` rows, so its records stay
    /// complete in metadata. What goes is the `.dat` and the segment's
    /// `(cid → segment)` routing rows — a block whose only copy was the
    /// evicted file then reads as dehydrated (absent, index row present),
    /// exactly as a loose block after [`Self::block_delete`].
    ///
    /// Rows first, file second (the reverse of adoption, for the same reason):
    /// a crash between leaves an unrouted file no reader reaches, never a
    /// routing row naming a missing file. `rows` in the result counts
    /// segments evicted.
    async fn segment_evict_dat(&self, scope: &str, target_bytes: u64) -> Result<RelayEvicted>;

    /// **Scope departure** (charter § The replica boundary, T2 transition 3):
    /// remove every trace of `scope` from this replica — its journal rows,
    /// state entries, frontier vector, record-index rows and their loose
    /// blocks, relay rows, and adopted segments (rows *and* files).
    ///
    /// Atomic-or-resumable, which is the whole reason this is a backend method
    /// rather than a loop over the ones above: the row deletions are one
    /// transaction, and the segment **files** — the part no transaction can
    /// cover — are swept after it commits, guarded by a durable pending mark
    /// the backend replays at open. A crash therefore leaves either "not
    /// dropped" or "dropped, files pending", never a half-departed scope
    /// (`nest/common.md` § Client-state recoverability).
    ///
    /// Idempotent: dropping a scope this replica does not hold reports zeros.
    async fn drop_scope(&self, scope: &str) -> Result<ScopeDropCounts>;

    /// Every scope holding at least one journal row by `writer`, ascending —
    /// the succession re-author's domain question ("where might this
    /// predecessor have un-pushed rows"). Deliberately journal-derived, not
    /// frontier-derived: a row appended offline on a fresh scope exists in
    /// the journal before any frontier row does, and missing it would strand
    /// exactly the tail the re-author exists to carry.
    async fn scopes_of_writer(&self, writer: &WriterId) -> Result<Vec<String>>;

    /// **Compact a RETIRED writer's carried journal rows, in ONE transaction**
    /// — the burnt-journal compaction (`account-replica-posture.md` § The
    /// store device principal, refinement 11; the authority is
    /// `account-data-plane.md` § Store logical schema: a writer may compact
    /// its own log's superseded class-2 rows). Each of `rows` is deleted only
    /// while its coordinate `(scope, writer, seq)` still holds exactly that
    /// row's op and item, and with it the relay-plane row recorded at the same
    /// coordinate. A row that no longer matches is skipped, never an error.
    ///
    /// Refuses the whole call, deleting nothing, when any row's writer is the
    /// store's stamped writer: a current writer's append counter reads its own
    /// log's maximum, so compacting its newest rows would re-issue coordinates
    /// the fleet may already hold. A retired writer never appends again (the
    /// append-time guard refuses it), so its freed coordinates stay free for
    /// the feed's own rows there.
    ///
    /// **Implementors:** the guard, every compare and every delete share one
    /// transaction.
    async fn compact_retired_rows(&self, rows: &[JournalRow]) -> Result<RetiredCompaction>;

    /// Every scope this replica has a frontier row for, ascending — "which
    /// scopes has this replica ever walked".
    ///
    /// The frontier plane is the honest answer to that question because it is
    /// the one plane a walked scope always reaches, empty or not. Its consumer
    /// is the departure seam's first-run seed
    /// (`fauna_sync_engine::departure`), which needs to know what a store
    /// already holds before it can claim anything left.
    async fn scopes_with_frontiers(&self) -> Result<Vec<String>>;

    // ── The outbox (W4, charter § The offline-mutation contract) ─────────────
    //
    // Its own durable component, `OfflineQueued` intents only (the phase-0
    // ruling). An undrained intent is the only copy of a pending write, so the
    // outbox is DELIBERATELY outside every scope-keyed plane: `drop_scope`
    // must never touch it. Additive at-rest: an older build ignores the table.

    /// Append a durable intent, atomically. The store assigns `channel_seq`
    /// (per-scope FIFO — append order) and `created_at`. Identity is
    /// `intent_id`: re-appending an id already present is the composer's
    /// crash-retry and writes nothing — reported as `false`.
    /// `local_writer`: the append-time writer guard, as on
    /// [`Self::insert_row`] — an intent is a local append (its payload may
    /// embed the writer identity), so a stale process must not enqueue one
    /// the succession's re-author walk has already run past.
    async fn outbox_append(
        &self,
        intent: &NewOutboxIntent,
        local_writer: Option<&WriterId>,
    ) -> Result<bool>;

    /// Every undrained intent, ordered `(scope, channel_seq)` ascending.
    /// Undrained = every row (an acked intent has no row — completion is
    /// deletion). Drain *policy* — parked-scope skip, backoff, the drainer
    /// split — is the engine's (`fauna_sync_engine::outbox`), never this
    /// layer's.
    async fn outbox_undrained(&self) -> Result<Vec<OutboxIntent>>;

    /// The ack arrived: delete the intent (completion-is-deletion). `false`
    /// if no such intent — the double-ack of a crash-replayed drain, a no-op.
    async fn outbox_ack(&self, intent_id: &[u8; 16]) -> Result<bool>;

    /// Permanent failure: set the intent's status to `failed` (parked,
    /// user-visible, never re-attempted without user action). The row stays —
    /// a park is not a drop. `false` if no such intent.
    async fn outbox_mark_failed(&self, intent_id: &[u8; 16]) -> Result<bool>;

    /// One drain attempt did not conclude: bump `retry_count`, stamp
    /// `last_attempt_at` (the `transfer_queue` backoff pair). `false` if no
    /// such intent.
    async fn outbox_record_attempt(&self, intent_id: &[u8; 16]) -> Result<bool>;
}

/// One `(scope, item_class)` family's relay-plane meter — the physical half of
/// `fauna_core::custody_policy::ScopeMeter`, which this converts into.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RelayScopeMeter {
    pub scope: String,
    pub item_class: String,
    /// Rows held, evicted-payload rows included — an evicted row still holds
    /// its coordinate floor and is still served as shape.
    pub rows: u64,
    /// Payload bytes at rest for this family.
    pub payload_bytes: u64,
    /// Rows whose payload may be dropped (payload present, `op` not floor).
    pub evictable_rows: u64,
    /// Bytes [`StoreBackend::relay_evict_payload`] could free here.
    pub evictable_bytes: u64,
}

/// What one [`StoreBackend::relay_evict_payload`] — or, counting segments as
/// its rows, one [`StoreBackend::segment_evict_dat`] — actually freed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RelayEvicted {
    pub rows: u64,
    pub bytes: u64,
}

/// One scope's adopted-segment meter — what [`StoreBackend::segment_meter`]
/// reports, and what `AccountStore::custody_meter` folds into a
/// `fauna_core::custody_policy::SEGMENT_ITEM_CLASS` family.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SegmentScopeMeter {
    pub scope: String,
    /// Segment rows held, evicted ones included — an evicted segment still
    /// holds its `.meta` floor.
    pub segments: u64,
    /// The subset of [`Self::segments`] whose `.dat` is still held.
    pub held_segments: u64,
    /// `.dat` bytes still held — the evictable part.
    pub dat_bytes: u64,
    /// `.meta` sidecar bytes — the floor, held for every segment row.
    pub meta_bytes: u64,
}

/// What one [`StoreBackend::compact_retired_rows`] deleted.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RetiredCompaction {
    /// Journal rows deleted.
    pub journal_rows: usize,
    /// Relay-plane rows deleted at those same coordinates.
    pub relay_rows: usize,
}

/// What one [`StoreBackend::drop_scope`] removed. Reported rather than logged
/// so a caller can assert the drop actually reached each plane — a departure
/// that silently removed nothing is the failure this counts against.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ScopeDropCounts {
    pub journal_rows: u64,
    pub state_entries: u64,
    pub frontier_rows: u64,
    pub record_index_rows: u64,
    pub blocks: u64,
    pub relay_rows: u64,
    pub segments: u64,
}

impl ScopeDropCounts {
    /// Whether anything at all left the store — the "this replica held the
    /// scope" question, asked without naming a plane.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Total rows removed across every plane (segment *files* are counted by
    /// [`Self::segments`], one per adopted pair).
    pub fn total(&self) -> u64 {
        self.journal_rows
            + self.state_entries
            + self.frontier_rows
            + self.record_index_rows
            + self.blocks
            + self.relay_rows
            + self.segments
    }
}
