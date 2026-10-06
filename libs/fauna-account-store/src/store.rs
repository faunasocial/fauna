//! The logical account store — the invariants, written once over any
//! [`StoreBackend`] (charter: `account-data-plane.md` § The account store +
//! § Store logical schema).
//!
//! Key-less by construction (R7 (account-data-plane.md § The ratified decisions)): nothing in this API takes or holds key
//! material; entry values and item refs are opaque bytes. Projections — the
//! only key-bearing plane — live above this crate and are never load-bearing
//! for store integrity.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use fauna_core::data::ContentHash;
use fauna_core::format::hex_full;

use fauna_core::custody_policy::{
    CustodyEvictionPlan, CustodyMeter, SEGMENT_ITEM_CLASS, ScopeMeter,
};

use crate::backend::{RelayEvicted, ScopeDropCounts, SegmentStaging, StoreBackend};
use crate::segments::{
    SegmentAdmission, SegmentHalf, SegmentKey, SegmentKindMismatch, SegmentSidecarView,
    SegmentSink, admit,
};
use crate::types::{
    Hydration, HydrationPolicy, InsertOutcome, IssuedRetire, ItemRef, JournalOp, JournalRow,
    NewOutboxIntent, OutboxIntent, RecordIndexEntry, RelayRow, StateEntry, WriterId,
};

/// The engine-singleton election lock's filename, reserved beside the store DB
/// (charter § Multi-instance concurrency, T9: the election IS a
/// kernel-arbitrated advisory lock on `<store dir>/engine.lock`).
/// [`crate::locks::EngineLock`] takes it; the name is owned here so no other
/// file squats on it.
pub const ENGINE_LOCK_FILENAME: &str = "engine.lock";

/// Where the engine-singleton election lock lives for a store rooted at
/// `store_dir`.
pub fn engine_lock_path(store_dir: &Path) -> PathBuf {
    store_dir.join(ENGINE_LOCK_FILENAME)
}

/// The migration/adoption critical section's lock filename, the charter's
/// *second* exclusive section (§ Multi-instance concurrency: "store-level
/// advisory locks for the two genuinely exclusive critical sections — schema
/// migration/adoption, and the engine-singleton role").
/// [`crate::locks::MigrationLock`] takes it; the name is owned here for the
/// same reason its sibling is.
///
/// **A separate file from `engine.lock`, deliberately.** One file for both
/// would make every cold open block behind whichever process holds the
/// engine role *for the role's whole lifetime* — the two sections are
/// unrelated, and their locks have opposite shapes (try vs. blocking).
pub const MIGRATION_LOCK_FILENAME: &str = "migration.lock";

/// Where the migration/adoption critical-section lock lives for a store
/// rooted at `store_dir`.
pub fn migration_lock_path(store_dir: &Path) -> PathBuf {
    store_dir.join(MIGRATION_LOCK_FILENAME)
}

/// The seed-leg role's lock filename, the store's *third* exclusive section
/// (`account-runtime.md` § Multi-instance concurrency → *The seed-leg role*,
/// part 1: one co-located seed-holding runtime runs the legs only a signed-in
/// app can run). [`crate::locks::SeedLegLock`] takes it; the name is owned
/// here for the same reason its siblings are.
///
/// **A separate file from `engine.lock`**: the two roles are won by different
/// processes in the steady state the role exists for — the seedless agent
/// pumps, the app beside it runs the seed-only legs.
pub const SEED_LEGS_LOCK_FILENAME: &str = "seed-legs.lock";

/// Where the seed-leg role's lock lives for a store rooted at `store_dir`.
pub fn seed_legs_lock_path(store_dir: &Path) -> PathBuf {
    store_dir.join(SEED_LEGS_LOCK_FILENAME)
}

/// How many entries the retire record keeps
/// ([`AccountStore::record_issued_retire`]) — the newest; older ones are
/// dropped. A constant, not a knob: no human chooses it. Sized for a machine
/// whose agent pumps for weeks with no app beside it to read the record: a
/// retire asked again at the same coordinates replaces its entry, so the count
/// is of distinct rows retired, and what falls off the end is a retire a
/// later pass re-makes, or a redundant row left at a secondary
/// (`account-sync-plane.md` § The bind leg, rulings 5 and 6).
pub const ISSUED_RETIRES_CAP: u32 = 2048;

/// The store's at-rest format version — the two-number scheme of
/// `version-compatibility.md` § 2.2 (the `schema_meta` / segment-store
/// precedent). Bump on every format change; bump the min-reader only on a
/// non-additive change.
///
/// **2 (2026-08-12):** the journal PK became `(scope, writer_id, writer_seq)`
/// (the genesis carries it; the v1-store rebuild that shipped with it was
/// retired by the 2026-09-24 compat-remnant sweep). Min-reader stays 1: a format-1 binary reads every row of a rebuilt store
/// correctly; the one degradation needs a store holding several content
/// scopes' records — a state the old binary could never *create* (its own
/// key refused the second scope, the bug this fixes) — and there its content
/// walks fail loudly per pass rather than misreading anything.
pub const FORMAT_VERSION: u16 = 2;
pub const MIN_READER_FORMAT_VERSION: u16 = 1;

/// The floor can never exceed what this binary writes — a build that cannot
/// read its own output is incoherent, and every open would refuse the pair it
/// just stamped. Compile-time so raising the floor without the format fails
/// the build, not a test run.
const _: () = assert!(MIN_READER_FORMAT_VERSION <= FORMAT_VERSION);

pub(crate) const META_FORMAT_VERSION: &str = "format_version";
pub(crate) const META_MIN_READER: &str = "min_reader_format_version";
const META_ACTOR_ID: &str = "actor_id";
pub(crate) const META_WRITER_ID: &str = "writer_id";
const META_HYDRATION_POLICY: &str = "hydration_policy";

// Why the block plane did NOT bump `FORMAT_VERSION`: the W2 (account-data-plane.md § Workstreams) contract's compat
// obligation 3 bumps it "only if an older reader would misread", and an older
// reader cannot — `record_index` and `blocks` are new tables created by
// `CREATE TABLE IF NOT EXISTS`, so an older binary opening this store never
// looks at them, and this binary opening an older store creates them empty.
// Purely additive in both directions, exactly the case the obligation exempts.

/// How many times a local append retries a lost `(writer, seq)` race before
/// giving up. Same-replica processes share one writer id (charter
/// § Multi-instance concurrency: non-singleton instances are plain
/// readers/writers), so the race is legal and bounded-retry is the resolution.
const APPEND_RETRY_LIMIT: u32 = 64;

/// The store cannot be opened by this binary: its `min_reader_format_version`
/// is newer than what this binary writes. Typed so callers can render the
/// honest "this app is too old" rather than "corrupt" (the
/// `ConfigStoreError::Incompatible` precedent). Nothing was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreIncompatible {
    pub format_version: u16,
    pub min_reader: u16,
    pub binary: u16,
}

impl std::fmt::Display for StoreIncompatible {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "account store format is newer than this binary: store format_version {}, \
             min_reader_format_version {}, binary writes {}",
            self.format_version, self.min_reader, self.binary
        )
    }
}

impl std::error::Error for StoreIncompatible {}

/// A local append found the store's writer identity re-stamped under it: this
/// handle's writer was rotated away (principal succession — charter § The
/// store device principal → *Principal succession after a device delete*,
/// decision 4). Typed so a stale co-located process can tell "reassemble and
/// adopt the successor" from corruption. Nothing was written.
///
/// Raised only by **guarded** local appends (the append paths that stamp this
/// store handle's own writer); ingest of another writer's rows is never
/// guarded — an origin's row is valid whoever this replica's writer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StaleWriter {
    /// The writer this store handle appends as.
    pub held: WriterId,
    /// The store's current writer — `META_WRITER_ID` read inside the append
    /// transaction.
    pub current: WriterId,
}

impl std::fmt::Display for StaleWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "local append refused: this handle's writer {} was rotated away \
             (the store's writer is now {}) — reassemble to adopt the successor",
            self.held.to_hex(),
            self.current.to_hex()
        )
    }
}

impl std::error::Error for StaleWriter {}

/// True when `e`'s chain carries a [`StaleWriter`] refusal — the one check a
/// caller needs before deciding "reassemble" (the refusal can surface behind
/// arbitrary `context` layers).
pub fn is_stale_writer(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.downcast_ref::<StaleWriter>().is_some())
}

/// How some row's author relates to one store's own identity —
/// [`AccountStore::writer_relation`]'s answer, read live from the store meta.
///
/// The distinction between the two own arms is load-bearing on a walk: the
/// CURRENT writer's frontier slot doubles as its published-to-the-feed
/// high-water, so a peer echoing that writer's row must not advance it, while
/// a RETIRED writer's slot is plain "applied through" accounting — nothing
/// publishes as a retired identity again — and stays transport-independent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriterRelation {
    /// The store's current writer: the identity this replica authors as now.
    Current,
    /// An identity this replica authored as before a rotation retired it. Its
    /// rows on any feed are this machine's own echoed history.
    Retired,
    /// Another replica's writer.
    Foreign,
}

impl WriterRelation {
    /// Either own arm — the store holds these rows as its own history rather
    /// than ingesting them.
    #[must_use]
    pub fn is_own(self) -> bool {
        !matches!(self, WriterRelation::Foreign)
    }
}

/// The tail re-author's durable memory: the PREDECESSOR writer a rotation
/// displaced, stamped **atomically with** the [`META_WRITER_ID`] re-stamp and
/// cleared only once the un-pushed tail has re-journaled under the successor
/// ([`clear_writer_reauthor_if_unchanged`]). A crash anywhere between the fence and
/// re-author leaves this marker in place, so the walk is resumable rather
/// than one-shot — the crash-safety half of succession decision 3.
const META_PRIOR_WRITER_ID: &str = "prior_writer_id";

/// The PERMANENT memory of every writer this store retired (concatenated
/// 32-byte ids, append-only, stamped atomically with each fence). Unlike the
/// re-author marker this never clears: a walk meeting a retired own writer's
/// row on the feed must treat it as its own echoed history — the store holds
/// those rows in their LOCALLY-AUTHORED form, whose `item_ref` encodes the
/// local entry counter, while an ingest re-derives the wire form (whose
/// `entry_version` is the origin seq, `ItemRef::StateKey`'s provenance rule)
/// — so re-ingesting them false-equivocates against the machine's own
/// history forever (found by V13, 2026-08-15).
const META_RETIRED_WRITERS: &str = "retired_writers";

/// The re-author marker's overflow — every EARLIER predecessor whose tail
/// was still pending when a later fence landed (concatenated 32-byte ids).
/// Two rotations with no pump pass between them (a crash window, or a lost
/// slot right after a revocation rotation — the lost-slot arm, 2026-08-27)
/// would otherwise overwrite [`META_PRIOR_WRITER_ID`] and orphan the
/// earlier tail silently. The overflow key holds the earlier predecessors
/// beside the newest one in the original key; the pass walks both and clears both ([`pending_writer_reauthors`]).
const META_PRIOR_WRITER_IDS_MORE: &str = "prior_writer_ids_more";

/// The walk's durable verdict that the CURRENT writer's journal is **burnt**
/// (`account-replica-posture.md` § The store device principal, refinement
/// 11): a feed served a row under this store's own current writer that the
/// journal does not hold — above what it holds, or at a held coordinate under
/// a different item. A writer lives exactly as long as its journal, so this
/// journal is not the one that authored those rows (a store dir restored from
/// an older backup), and every seq it issues next may collide with history the
/// fleet already holds. The assembly's heal arm reads it beside the stamp
/// (`stamped_and_burnt_writer`) and rotates the burnt writer away through the
/// ordinary fence — the heal's reading is inert once the stamp no longer
/// names it, which is why it needs no clear. After that fence it still names
/// the predecessor, and that is its second reader: the tail re-author
/// ([`ReauthorMarker::burnt`]) compacts a burnt predecessor's carried rows
/// once their entries re-put under the successor (refinement 11's compaction,
/// [`crate::backend::StoreBackend::compact_retired_rows`]).
const META_BURNT_WRITER_ID: &str = "burnt_writer_id";

/// The store's memory that its writer **retired itself at a sign-out**
/// (`account-replica-posture.md` § The store device principal → *Principal
/// succession after a device delete*, decision 1, the third trigger → *A
/// sign-out is not a removal*). A sign-out writes this machine's own
/// `Removed` device-set row, and that row is exactly what a seed-holding
/// runtime mints a successor on — so the sign-out stamps the writer here
/// **before** it writes the row, and every runtime on this store reads the
/// pair as "signed out", never as "removed by the fleet". Local and never
/// published, which is the point: the row's own `removed_by` is a claim any
/// enrolled device can write, and this is not. Erased with the store; a
/// seed-holding host start clears it (a sign-in outranks a sign-out whose
/// erase did not land); inert once the store's writer is no longer the one
/// it names.
const META_SIGNED_OUT_WRITER_ID: &str = "signed_out_writer_id";

/// Re-stamp the store's writer identity from `from` to `to` — the principal
/// succession's **fence** (charter § The store device principal → *Principal
/// succession after a device delete*, decision 2). The caller MUST hold the
/// store's migration critical section ([`MIGRATION_LOCK_FILENAME`]): this is
/// a probe-then-act on shared meta, and the lock is its only serialization.
///
/// Ordering contract: from the moment the re-stamp lands, every guarded local
/// append by a `from`-holding handle refuses typed ([`StaleWriter`]), so a
/// tail walk running after it can never miss a late old-writer row — which is
/// exactly why the rotation re-stamps FIRST and re-authors after.
///
/// The fence also records `from` as the pending-re-author marker
/// ([`pending_writer_reauthor`]) **in the same transaction** — landing the
/// fence without the marker would strand the un-pushed tail silently, which
/// is precisely the no-data-loss failure the ruling's conservative bound
/// exists to prevent.
///
/// Idempotent against a racing successor: a store already stamped `to`
/// reports `Ok` and leaves the racing rotator's marker untouched. A store
/// stamped neither key refuses — the caller's picture of the predecessor is
/// stale, and guessing here would orphan a third identity's log.
pub async fn rotate_writer_identity<B: StoreBackend>(
    backend: &B,
    from: &WriterId,
    to: &WriterId,
) -> Result<()> {
    match backend.meta_get(META_WRITER_ID).await? {
        // No store identity = a machine with no store yet (a revoked FRESH
        // enrollment, or a store dir the user wiped): the fence is VACUOUS —
        // there is no journal to fence and no tail to re-author, and the
        // fresh store adopts the successor at its first open. Deliberately
        // not an error: failing here would wedge exactly the revival this
        // rotation exists for. (A wrong-store-dir caller stays loud anyway:
        // the REAL store's meta keeps the dead writer, so the next open
        // under the successor refuses typed at `adopt_identity`.)
        None => Ok(()),
        Some(v) if v.as_slice() == to.0 => Ok(()),
        Some(v) if v.as_slice() == from.0 => {
            // Append `from` to the permanent retired-writers memory, in the
            // SAME transaction as the fence (read-modify-write is safe here:
            // the caller holds the migration lock). Idempotent append.
            let mut retired = backend
                .meta_get(META_RETIRED_WRITERS)
                .await?
                .unwrap_or_default();
            if !retired.chunks(32).any(|c| c == from.0) {
                retired.extend_from_slice(&from.0);
            }
            // An EARLIER rotation's marker still pending (no pump pass ran
            // between the two fences) is preserved in the overflow key, never
            // overwritten: its tail is still owed. Idempotent append.
            let mut more = backend
                .meta_get(META_PRIOR_WRITER_IDS_MORE)
                .await?
                .unwrap_or_default();
            if let Some(prior) = backend.meta_get(META_PRIOR_WRITER_ID).await?
                && prior.as_slice() != from.0
                && !more.chunks(32).any(|c| c == prior.as_slice())
            {
                more.extend_from_slice(&prior);
            }
            let mut pairs: Vec<(&str, &[u8])> = vec![
                (META_WRITER_ID, &to.0),
                (META_PRIOR_WRITER_ID, &from.0),
                (META_RETIRED_WRITERS, &retired),
            ];
            if !more.is_empty() {
                pairs.push((META_PRIOR_WRITER_IDS_MORE, &more));
            }
            backend.meta_put_all(&pairs).await
        }
        Some(v) => bail!(
            "writer rotation: the store's writer is {} — neither the expected \
             predecessor {} nor the successor {}",
            hex_full(&v),
            from.to_hex(),
            to.to_hex()
        ),
    }
}

/// Stamp a store **no writer has opened yet** with `successor`, recording
/// `dead` — the key the slot held over it — as retired: the inverse arm of the
/// lost-slot fence (`account-replica-posture.md` § The store device
/// principal, refinement 11). A writer lives exactly as long as its journal,
/// so a key LOADED from the slot over a store with no stamped writer has a
/// history this journal cannot vouch for, and it is abandoned rather than put
/// to work: the fresh store starts as `successor`, and `dead` joins the
/// retired-writers memory so its rows on any feed read as this machine's own
/// retired history, and so a slot that comes back holding it (a credential
/// store not retaining the successor's write) meets the RETIRED-in-slot arm
/// instead of being fenced back onto a burnt key. No re-author marker: there
/// is no journal to re-author.
///
/// The caller MUST hold the migration critical section (a probe-then-act on
/// shared meta). Idempotent against a racing sibling that already stamped
/// `successor`; a store stamped anything else refuses — this is the
/// fresh-store arm, and a stamped store is the fence's business.
pub async fn retire_unjournaled_writer<B: StoreBackend>(
    backend: &B,
    dead: &WriterId,
    successor: &WriterId,
) -> Result<()> {
    match backend.meta_get(META_WRITER_ID).await? {
        None => {
            let mut retired = backend
                .meta_get(META_RETIRED_WRITERS)
                .await?
                .unwrap_or_default();
            if !retired.chunks(32).any(|c| c == dead.0) {
                retired.extend_from_slice(&dead.0);
            }
            backend
                .meta_put_all(&[
                    (META_WRITER_ID, &successor.0),
                    (META_RETIRED_WRITERS, &retired),
                ])
                .await
        }
        Some(v) if v.as_slice() == successor.0 => Ok(()),
        Some(v) => bail!(
            "retire unjournaled writer: the store is already stamped {} — a stamped store \
             is fenced (`rotate_writer_identity`), never re-stamped from scratch",
            hex_full(&v)
        ),
    }
}

/// The stamped writer beside the walk's burnt verdict, read in ONE
/// transaction (`meta_get_all`): the assembly's heal arm decides on the pair,
/// and a fence lands both keys' meaning together, so two reads could serve a
/// pre-fence stamp beside a verdict about the successor. `burnt` is `Some`
/// only while it still names the stamped writer — a verdict about a writer
/// the store has since rotated away is inert and reads as `None`.
pub async fn stamped_and_burnt_writer<B: StoreBackend>(
    backend: &B,
) -> Result<(Option<WriterId>, Option<WriterId>)> {
    let values = backend
        .meta_get_all(&[META_WRITER_ID, META_BURNT_WRITER_ID])
        .await?;
    let [stamped_raw, burnt_raw]: [Option<Vec<u8>>; 2] = values
        .try_into()
        .map_err(|v: Vec<_>| anyhow::anyhow!("meta_get_all returned {} values, want 2", v.len()))?;
    let stamped = stamped_raw
        .map(|v| {
            v.as_slice()
                .try_into()
                .map(WriterId)
                .context("store meta: the stamped writer id is not 32 bytes")
        })
        .transpose()?;
    let burnt = burnt_raw
        .map(|v| {
            v.as_slice()
                .try_into()
                .map(WriterId)
                .context("store meta: the burnt writer id is not 32 bytes")
        })
        .transpose()?
        .filter(|b| stamped.as_ref() == Some(b));
    Ok((stamped, burnt))
}

/// The walk's burnt verdict, RAW — the writer it names whether or not the
/// store is still stamped with it (where [`stamped_and_burnt_writer`] filters
/// to the stamp for the heal arm). After the fence it names the retired
/// predecessor whose journal the walk found burnt, and that is this reader's
/// purpose: a burnt journal's held coordinates vouch for nothing, so the walk
/// carries that writer's rows instead of echoing them
/// (`account-replica-posture.md` § The store device principal, refinement 11
/// → *the retired burnt writer's rows are carried*). `None` for a store no
/// walk ever found burnt.
pub async fn burnt_writer<B: StoreBackend>(backend: &B) -> Result<Option<WriterId>> {
    match backend.meta_get(META_BURNT_WRITER_ID).await? {
        None => Ok(None),
        Some(v) => {
            let id: [u8; 32] = v
                .as_slice()
                .try_into()
                .context("store meta: the burnt writer id is not 32 bytes")?;
            Ok(Some(WriterId(id)))
        }
    }
}

/// The writer this store is stamped with — `None` for a store no writer has
/// opened yet (its first open adopts the opener). The lost-slot arm's probe
/// (`principal_succession::lost_slot_heal`): a slot key that disagrees with
/// this is a store the slot no longer matches, healed by a fence rather
/// than refused by [`AccountStore::open`].
pub async fn stamped_writer<B: StoreBackend>(backend: &B) -> Result<Option<WriterId>> {
    match backend.meta_get(META_WRITER_ID).await? {
        None => Ok(None),
        Some(v) => {
            let id: [u8; 32] = v
                .as_slice()
                .try_into()
                .context("store meta: the stamped writer id is not 32 bytes")?;
            Ok(Some(WriterId(id)))
        }
    }
}

/// Every writer this store has retired through a rotation, oldest first.
/// Permanent — a retired identity's rows live on the fleet's feeds forever,
/// and every future walk must recognise them as this machine's own history.
pub async fn retired_writers<B: StoreBackend>(backend: &B) -> Result<Vec<WriterId>> {
    writer_id_list(backend, META_RETIRED_WRITERS, "retired-writers memory").await
}

/// A concatenated-32-byte-ids meta value as a list, in stored order.
async fn writer_id_list<B: StoreBackend>(
    backend: &B,
    key: &str,
    what: &str,
) -> Result<Vec<WriterId>> {
    writer_ids_from(backend.meta_get(key).await?.as_deref(), what)
}

/// The decoder half of [`writer_id_list`], over bytes already in hand — the
/// snapshot reader ([`pending_reauthor_snapshot`]) has them from one
/// transaction and must not go back to the backend for a second read.
fn writer_ids_from(raw: Option<&[u8]>, what: &str) -> Result<Vec<WriterId>> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    if raw.len() % 32 != 0 {
        bail!(
            "{what} is corrupt ({} bytes — not a multiple of 32)",
            raw.len()
        );
    }
    Ok(raw
        .chunks(32)
        .map(|c| WriterId(c.try_into().expect("chunked to 32")))
        .collect())
}

/// The predecessor writer whose un-pushed tail still awaits re-authoring
/// under the current writer — `Some` from the fence until
/// [`clear_writer_reauthor_if_unchanged`], across crashes.
pub async fn pending_writer_reauthor<B: StoreBackend>(backend: &B) -> Result<Option<WriterId>> {
    match backend.meta_get(META_PRIOR_WRITER_ID).await? {
        None => Ok(None),
        Some(v) => {
            let prior: [u8; 32] = v
                .as_slice()
                .try_into()
                .context("pending re-author: stored prior writer is not 32 bytes")?;
            Ok(Some(WriterId(prior)))
        }
    }
}

/// Every predecessor whose un-pushed tail still awaits re-authoring — the
/// newest first ([`pending_writer_reauthor`]), then the earlier ones a later
/// fence preserved in the overflow key. What the tail walk iterates.
pub async fn pending_writer_reauthors<B: StoreBackend>(backend: &B) -> Result<Vec<WriterId>> {
    let mut priors: Vec<WriterId> = pending_writer_reauthor(backend)
        .await?
        .into_iter()
        .collect();
    for older in writer_id_list(
        backend,
        META_PRIOR_WRITER_IDS_MORE,
        "pending re-author overflow",
    )
    .await?
    {
        if !priors.contains(&older) {
            priors.push(older);
        }
    }
    Ok(priors)
}

/// One consistent picture of the store's writer stamp and the pending
/// re-author marker — the pair [`rotate_writer_identity`] writes in a single
/// transaction, read back in a single transaction.
///
/// The re-author pass must decide on a snapshot, not on two reads: it filters
/// the marker's predecessors against the stamp, so a fence landing *between*
/// those reads yields a pre-fence stamp beside a post-fence marker, the
/// filter drops the marker's own predecessor as if it were the store's own
/// writer, and the pass then reads "nothing pending" over a marker that names
/// a real un-pushed tail (charter § The store device principal → succession
/// decision 3; the same loss closed for a stale handle's cached
/// writer, reached through a race instead of a cache).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReauthorMarker {
    /// The store's stamped writer as of the snapshot.
    pub stamped: Option<WriterId>,
    /// Every predecessor owed a tail walk as of the snapshot, newest first —
    /// the same order and de-duplication [`pending_writer_reauthors`] gives.
    pub priors: Vec<WriterId>,
    /// The walk's burnt verdict as of the snapshot — raw, NOT filtered to the
    /// stamp the way [`stamped_and_burnt_writer`] filters it: after the fence
    /// it names the predecessor whose journal the walk found burnt, and the
    /// pass compacts exactly that predecessor's carried rows (refinement 11).
    pub burnt: Option<WriterId>,
    /// The two marker keys' raw bytes exactly as read. Not for interpreting —
    /// this is the picture [`clear_writer_reauthor_if_unchanged`] re-asserts
    /// before it deletes anything.
    raw: [(&'static str, Option<Vec<u8>>); 2],
}

/// Read the writer stamp, both marker keys and the walk's burnt verdict as
/// one snapshot ([`ReauthorMarker`]).
pub async fn pending_reauthor_snapshot<B: StoreBackend>(backend: &B) -> Result<ReauthorMarker> {
    let keys = [
        META_WRITER_ID,
        META_PRIOR_WRITER_ID,
        META_PRIOR_WRITER_IDS_MORE,
        META_BURNT_WRITER_ID,
    ];
    let values = backend.meta_get_all(&keys).await?;
    let [stamped_raw, newest_raw, more_raw, burnt_raw]: [Option<Vec<u8>>; 4] = values
        .try_into()
        .map_err(|v: Vec<_>| anyhow::anyhow!("meta_get_all returned {} values, want 4", v.len()))?;

    let stamped = match &stamped_raw {
        None => None,
        Some(v) => {
            Some(WriterId(v.as_slice().try_into().context(
                "store meta: the stamped writer id is not 32 bytes",
            )?))
        }
    };
    let mut priors: Vec<WriterId> = match &newest_raw {
        None => Vec::new(),
        Some(v) => {
            vec![WriterId(v.as_slice().try_into().context(
                "pending re-author: stored prior writer is not 32 bytes",
            )?)]
        }
    };
    for older in writer_ids_from(more_raw.as_deref(), "pending re-author overflow")? {
        if !priors.contains(&older) {
            priors.push(older);
        }
    }
    let burnt = burnt_raw
        .map(|v| {
            v.as_slice()
                .try_into()
                .map(WriterId)
                .context("store meta: the burnt writer id is not 32 bytes")
        })
        .transpose()?;
    Ok(ReauthorMarker {
        stamped,
        priors,
        burnt,
        raw: [
            (META_PRIOR_WRITER_ID, newest_raw),
            (META_PRIOR_WRITER_IDS_MORE, more_raw),
        ],
    })
}

/// The re-author walk completed: drop the marker — both keys — **but only if
/// they still hold exactly what `snapshot` read**. `Ok(false)` means a fence
/// landed under us between the decision and this delete: the marker now names
/// a tail this pass never walked, so it stays for the next pass, which reads
/// a fresh snapshot and walks it (re-walking an already-walked predecessor is
/// idempotent by decision 3's own argument — a re-put of the same value under
/// the same `merge_meta` — so the conservative outcome costs nothing but a
/// pass).
///
/// One transaction, unlike the unconditional two-delete clear it replaces: a
/// compare-and-delete split in half is the very race it exists to close. A
/// crash between deciding and deleting still leaves the marker for the next
/// pass, which is the same crash-resumability the old clear had.
pub async fn clear_writer_reauthor_if_unchanged<B: StoreBackend>(
    backend: &B,
    snapshot: &ReauthorMarker,
) -> Result<bool> {
    let expected: Vec<(&str, Option<&[u8]>)> = snapshot
        .raw
        .iter()
        .map(|(key, value)| (*key, value.as_deref()))
        .collect();
    backend.meta_delete_all_if_unchanged(&expected).await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FormatVerdict {
    Compatible,
    /// Stored format is newer but still readable by this binary — tolerate,
    /// and never restamp the pair down (the nest `schema_meta` law).
    NewerAdditive,
    NewerBreaking,
}

pub(crate) fn check_format_compatibility(
    stored_v: u16,
    stored_min: u16,
    binary_v: u16,
) -> FormatVerdict {
    if stored_min > binary_v {
        FormatVerdict::NewerBreaking
    } else if stored_v > binary_v {
        FormatVerdict::NewerAdditive
    } else {
        FormatVerdict::Compatible
    }
}

/// The logical store over a physical backend. One value per replica.
pub struct AccountStore<B: StoreBackend> {
    backend: B,
    actor_id_hex: String,
    writer: WriterId,
    /// Writers this store retired through rotations, cached at open — a
    /// walk consults it per row, and any process whose picture is stale is
    /// fenced into reassembly (and a fresh open) by the append-time guard
    /// before its staleness can matter.
    retired: Vec<WriterId>,
    /// How many times one of this value's entry-write doors changed what a
    /// read can answer — see [`Self::entry_changes`].
    entry_changes: std::sync::atomic::AtomicU64,
}

impl<B: StoreBackend> std::fmt::Debug for AccountStore<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountStore")
            .field("actor_id_hex", &self.actor_id_hex)
            .field("writer", &self.writer.to_hex())
            .finish_non_exhaustive()
    }
}

impl<B: StoreBackend> AccountStore<B> {
    /// Open the store: verify the format-version pair (stamping a fresh
    /// store), then verify replica identity (stamping a fresh store) — an
    /// existing store bound to a different actor or writer refuses to open
    /// rather than silently interleaving two identities' logs.
    pub async fn open(backend: B, actor_id_hex: &str, writer: WriterId) -> Result<Self> {
        verify_and_stamp_format(&backend, FORMAT_VERSION, MIN_READER_FORMAT_VERSION).await?;

        adopt_identity(&backend, META_ACTOR_ID, actor_id_hex.as_bytes(), "actor").await?;
        adopt_identity(&backend, META_WRITER_ID, &writer.0, "writer").await?;

        let retired = retired_writers(&backend).await?;
        Ok(Self {
            backend,
            actor_id_hex: actor_id_hex.to_string(),
            writer,
            retired,
            entry_changes: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// How many times, since this value was opened, one of its entry-write
    /// doors changed an entry a read can answer: a state entry (account or
    /// group plane) put, ingested with a different value, or forgotten; a
    /// record staged, noted, tombstoned or adopted in a segment; a scope
    /// dropped. Bookkeeping writes — journal rows alone, frontiers,
    /// watermarks, relay rows, the outbox — never count.
    ///
    /// This value's own writes only: another connection's commit on the same
    /// store moves `data_version`, never this. The account runtime reads it
    /// around every run of its pump to move the change generation
    /// (`account-runtime.md` § Multi-instance concurrency (W5) → *A runtime's
    /// own pump is a source of the notice too*, part 1). A plain atomic read.
    pub fn entry_changes(&self) -> u64 {
        self.entry_changes
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    fn entry_changed(&self) {
        self.entry_changes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// A state entry landed over `held`: it counts when a read of it now
    /// answers differently — a new entry, another value, a tombstone set or
    /// lifted. A re-put of the value already held moves only its version.
    fn note_entry_write(&self, held: Option<&StateEntry>, landed: &StateEntry) {
        if held.is_none_or(|h| h.value != landed.value || h.tombstone != landed.tombstone) {
            self.entry_changed();
        }
    }

    pub fn actor_id_hex(&self) -> &str {
        &self.actor_id_hex
    }

    pub fn writer(&self) -> WriterId {
        self.writer
    }

    /// Writers this store retired through rotations (principal succession),
    /// oldest first — their rows on any feed are this machine's own echoed
    /// history, held locally in authored form, never to be re-ingested.
    pub fn retired_writers(&self) -> &[WriterId] {
        &self.retired
    }

    /// How `writer` relates to **this store's identity as it stands right
    /// now** — the live answer [`Self::writer`] and [`Self::retired_writers`]
    /// cannot give.
    ///
    /// Both of those are snapshots taken at [`Self::open`] and never
    /// reassigned, because [`rotate_writer_identity`] takes the backend, not
    /// the store: **no open handle ever learns of a fence.** So after a
    /// co-located sibling fences `A -> B`, an `A`-handle still answers `A` for
    /// its writer and `[]` for its retired set — and any decision keyed on
    /// that pair answers a question about a store that no longer exists. The
    /// dangerous direction is the store's OWN current writer reading back as
    /// [`WriterRelation::Foreign`]: a walk then treats this replica's own rows
    /// as another replica's (charter § The peer leg).
    ///
    /// Costs one backend transaction, per decision. Callers on a per-row path
    /// pay that deliberately: a cheaper per-page or per-walk snapshot would be
    /// the same defect with a shorter window.
    pub async fn writer_relation(&self, writer: &WriterId) -> Result<WriterRelation> {
        // ONE transaction for both keys. The fence writes the stamp and the
        // retired set together, so reading them apart can serve a pre-fence
        // stamp beside a post-fence retired set — the successor reads
        // `Foreign` — or the reverse, where the predecessor does. Either tear
        // reintroduces, for its width, exactly what this method exists to
        // prevent, and no read order avoids it: the fix is not to have two
        // reads.
        let values = self
            .backend
            .meta_get_all(&[META_WRITER_ID, META_RETIRED_WRITERS])
            .await?;
        let [stamped_raw, retired_raw]: [Option<Vec<u8>>; 2] =
            values.try_into().map_err(|v: Vec<_>| {
                anyhow::anyhow!("meta_get_all returned {} values, want 2", v.len())
            })?;

        // No stamped identity at all: this store has never been fenced —
        // `rotate_writer_identity` is vacuous on a store with no
        // `META_WRITER_ID` and stamps nothing — so the open-time cache is the
        // live answer, and reading it here is not a staleness.
        let Some(stamped_raw) = stamped_raw else {
            return Ok(self.cached_writer_relation(writer));
        };
        let stamped = WriterId(
            stamped_raw
                .as_slice()
                .try_into()
                .context("store meta: the stamped writer id is not 32 bytes")?,
        );
        if stamped == *writer {
            return Ok(WriterRelation::Current);
        }
        Ok(
            if writer_ids_from(retired_raw.as_deref(), "retired-writers memory")?.contains(writer) {
                WriterRelation::Retired
            } else {
                WriterRelation::Foreign
            },
        )
    }

    /// The same classification against the `open`-time snapshot — correct only
    /// where a fence is unrepresentable (see [`Self::writer_relation`]'s `None`
    /// arm, its one caller).
    fn cached_writer_relation(&self, writer: &WriterId) -> WriterRelation {
        if self.writer == *writer {
            WriterRelation::Current
        } else if self.retired.contains(writer) {
            WriterRelation::Retired
        } else {
            WriterRelation::Foreign
        }
    }

    /// Record the walk's verdict that `writer`'s journal — this store's — is
    /// burnt ([`META_BURNT_WRITER_ID`]): a feed served an own-current row this
    /// journal does not hold. Durable so the heal survives a crash between
    /// the walk and the assembly that acts on it; idempotent. The heal is the
    /// assembly's (`stamped_and_burnt_writer`), never this handle's: a
    /// rotation takes the slot and the migration section, which a walk holds
    /// neither of.
    pub async fn mark_writer_burnt(&self, writer: &WriterId) -> Result<()> {
        self.backend.meta_put(META_BURNT_WRITER_ID, &writer.0).await
    }

    /// The walk's burnt verdict, raw ([`burnt_writer`]) — read LIVE, per
    /// decision, like [`Self::writer_relation`]: the fence that retires the
    /// burnt writer and the walk that reads the verdict may run in
    /// different handles.
    pub async fn burnt_writer(&self) -> Result<Option<WriterId>> {
        burnt_writer(&self.backend).await
    }

    /// Record that `writer` — this store's — is retiring itself at a sign-out
    /// ([`META_SIGNED_OUT_WRITER_ID`]). Stamped before the sign-out writes
    /// the machine's own `Removed` row, so no runtime on this store ever
    /// reads that row without it. Idempotent.
    pub async fn mark_writer_signed_out(&self, writer: &WriterId) -> Result<()> {
        self.backend
            .meta_put(META_SIGNED_OUT_WRITER_ID, &writer.0)
            .await
    }

    /// The writer a sign-out retired on this store, if one stands — read
    /// LIVE, like [`Self::burnt_writer`]: the runtime that signs out and the
    /// one that reads the verdict may be different handles on one store.
    pub async fn signed_out_writer(&self) -> Result<Option<WriterId>> {
        match self.backend.meta_get(META_SIGNED_OUT_WRITER_ID).await? {
            None => Ok(None),
            Some(v) => {
                let id: [u8; 32] = v
                    .as_slice()
                    .try_into()
                    .context("store meta: the signed-out writer id is not 32 bytes")?;
                Ok(Some(WriterId(id)))
            }
        }
    }

    /// Clear the sign-out's memory — a seed-holding host start's act: the
    /// user is signed in on this store again (or still: the sign-out never
    /// reached its erase), so its own `Removed` row is once more a removal to
    /// heal from.
    pub async fn clear_signed_out_writer(&self) -> Result<()> {
        self.backend.meta_delete(META_SIGNED_OUT_WRITER_ID).await
    }

    /// Direct backend access for layers this slice deliberately does not
    /// build (W2 sync, W3 read paths). Invariant-preserving callers only.
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// Journal a class-1 `record-added` on the local writer's log. Returns
    /// the assigned seq (monotonic, gapless per writer).
    pub async fn append_record_added(&self, scope: &str, cid: ContentHash) -> Result<u64> {
        self.append_local(scope, JournalOp::RecordAdded, ItemRef::Cid(cid))
            .await
    }

    /// Journal a tombstone on the local writer's log.
    pub async fn append_tombstone(&self, scope: &str, item: ItemRef) -> Result<u64> {
        self.append_local(scope, JournalOp::Tombstone, item).await
    }

    /// Write a class-2 state entry: the entry upsert and its `state-put`
    /// journal row land in one backend transaction (charter component 3: the
    /// entry is the reconcile unit, journal rows are its incremental
    /// transport — one must never exist without the other). The store assigns
    /// `entry_version` = stored version + 1 and the journal seq; the caller's
    /// `entry.entry_version` is ignored. Returns `(entry_version, seq)`.
    ///
    /// Both counters are read outside the write's transaction and re-read on
    /// every try: another instance on the same store may take the seq or move
    /// the entry in between, and the backend refuses either rather than land
    /// a second row at one coordinate or a second value at one version.
    pub async fn put_state(&self, mut entry: StateEntry) -> Result<(u64, u64)> {
        for _ in 0..APPEND_RETRY_LIMIT {
            let held = self.backend.state_get(&entry.kind, &entry.key).await?;
            let next_version = held.as_ref().map_or(1, |e| e.entry_version + 1);
            entry.entry_version = next_version;
            let seq = self.next_local_seq().await?;
            let row = JournalRow {
                writer: self.writer,
                seq,
                scope: entry.scope.clone(),
                op: JournalOp::StatePut,
                item: ItemRef::StateKey {
                    kind: entry.kind.clone(),
                    key: entry.key.clone(),
                    entry_version: next_version,
                },
            };
            match self
                .backend
                .state_put_with_row(&entry, &row, Some(&self.writer))
                .await?
            {
                InsertOutcome::Inserted => {
                    self.note_entry_write(held.as_ref(), &entry);
                    return Ok((next_version, seq));
                }
                // Another same-writer process took the seq (or replayed an
                // identical row there), or moved the entry: re-read and retry.
                InsertOutcome::OccupiedByDifferent
                | InsertOutcome::IdenticalPresent
                | InsertOutcome::EntryMoved => continue,
            }
        }
        bail!(
            "state put lost the writer-seq race {APPEND_RETRY_LIMIT} times \
             (writer {})",
            self.writer.to_hex()
        )
    }

    /// Apply another writer's class-2 entry: that writer's journal row **and**
    /// the value this replica decided to hold for `(kind, key)`, in one
    /// transaction — the ingest twin of [`Self::put_state`].
    ///
    /// The two halves are deliberately different in provenance: `row` is the
    /// origin's, stored verbatim like any ingested row (charter § Store logical
    /// schema component 2), while `entry` is **this** replica's merged current
    /// value (component 3) — the merge seam's output, not the incoming value
    /// echoed. `entry.entry_version` is ignored and reassigned from this
    /// store's own counter, exactly as [`Self::put_state`] does.
    ///
    /// Idempotent on replay: a byte-identical row already at those coordinates
    /// reports [`InsertOutcome::IdenticalPresent`] and writes nothing (so the
    /// full-state reconcile can re-present every row without churning the entry
    /// table). A *different* row there is equivocation and is refused loudly,
    /// same as [`Self::ingest_row`].
    ///
    /// Callers whose merge decided to keep the local value use
    /// [`Self::ingest_row`] instead: the row still has to land (it is what the
    /// frontier accounts against), the entry must not move.
    pub async fn ingest_state(
        &self,
        row: &JournalRow,
        mut entry: StateEntry,
    ) -> Result<InsertOutcome> {
        let mut outcome = InsertOutcome::EntryMoved;
        for _ in 0..APPEND_RETRY_LIMIT {
            let held = self.backend.state_get(&entry.kind, &entry.key).await?;
            entry.entry_version = held.as_ref().map_or(1, |e| e.entry_version + 1);
            outcome = self.backend.state_put_with_row(&entry, row, None).await?;
            if outcome == InsertOutcome::Inserted {
                self.note_entry_write(held.as_ref(), &entry);
            }
            if outcome != InsertOutcome::EntryMoved {
                break;
            }
        }
        match outcome {
            InsertOutcome::EntryMoved => bail!(
                "state ingest lost the entry-version race {APPEND_RETRY_LIMIT} times \
                 (kind {:?} key {:?})",
                entry.kind,
                entry.key
            ),
            InsertOutcome::OccupiedByDifferent => {
                // Error-path-only second read: name BOTH sides — an
                // equivocation report that names only coordinates cost a
                // debugging session (V13, 2026-08-15).
                let occupant = self
                    .backend
                    .rows_for_scope(&row.scope, &row.writer, row.seq.saturating_sub(1), 1)
                    .await
                    .ok()
                    .and_then(|rows| rows.into_iter().find(|r| r.seq == row.seq));
                bail!(
                    "journal equivocation: scope {:?} writer {} seq {} already held with \
                     different content — refusing to overwrite (incoming: op {:?} item {:?}; \
                     held: {:?})",
                    row.scope,
                    row.writer.to_hex(),
                    row.seq,
                    row.op.as_str(),
                    row.item,
                    occupant.map(|r| (r.op.as_str().to_string(), r.item))
                )
            }
            ok => Ok(ok),
        }
    }

    /// Store another replica's row verbatim (charter component 2: "other
    /// writers' rows arrive via sync and are stored verbatim"). Idempotent on
    /// identical replay. Gaps in the origin's seq run are LEGAL (the origin
    /// may have compacted its own log; the entry reconcile is the backstop).
    /// A *different* row at already-held coordinates is refused loudly:
    /// journal rows are immutable, so divergence is equivocation or
    /// corruption, never something to paper over.
    pub async fn ingest_row(&self, row: &JournalRow) -> Result<InsertOutcome> {
        match self.backend.insert_row(row, None).await? {
            InsertOutcome::OccupiedByDifferent => bail!(
                "journal equivocation: scope {:?} writer {} seq {} already held with \
                 different content — refusing to overwrite (incoming: op {:?} item {:?})",
                row.scope,
                row.writer.to_hex(),
                row.seq,
                row.op.as_str(),
                row.item
            ),
            ok => Ok(ok),
        }
    }

    /// The scope-feed coordinate this replica journaled for `item` in `scope`
    /// — what the T1 browse intake itemizes into the seen-set. `None` when the
    /// scope's journal holds no row for the item, which the intake treats as
    /// *not yet resolvable* (drop and re-record on a later render).
    ///
    /// See [`StoreBackend::coordinate_of_item`] for the coordinate choice.
    pub async fn coordinate_of_item(
        &self,
        scope: &str,
        item: &ItemRef,
    ) -> Result<Option<(WriterId, u64)>> {
        self.backend.coordinate_of_item(scope, item).await
    }

    /// The frontier vector for `scope` (charter § The frontier vector),
    /// sorted by writer for deterministic reads.
    pub async fn frontier(&self, scope: &str) -> Result<Vec<(WriterId, u64)>> {
        let mut v = self.backend.frontier(scope).await?;
        v.sort();
        Ok(v)
    }

    /// Advance `writer`'s high-water in `scope` to `seq`. Two laws (charter
    /// § The frontier vector):
    ///
    /// - **Accounted walk only:** `seq` must be ≤ the highest row this store
    ///   actually holds for `(scope, writer)` — a high-water asserts every
    ///   row ≤ it was applied or deliberately skipped, which is unassertable
    ///   about rows never received.
    /// - **Never regress:** an advance to ≤ the current high-water is a no-op
    ///   (normal catch-up replay), enforced by the backend's atomic
    ///   MAX-merge. Returns the resulting high-water.
    pub async fn advance_frontier(&self, scope: &str, writer: &WriterId, seq: u64) -> Result<u64> {
        let held = self.backend.max_scope_writer_seq(scope, writer).await?;
        match held {
            Some(max) if seq <= max => self.backend.frontier_raise(scope, writer, seq).await,
            _ => bail!(
                "unaccounted frontier advance: scope {scope:?} writer {} to seq {seq}, \
                 but the store holds rows only up to {held:?}",
                writer.to_hex()
            ),
        }
    }

    /// The highest seq this store actually **holds** on `writer`'s log within
    /// `scope` — the bound [`Self::advance_frontier`] enforces, exposed so a
    /// caller can ask before advancing instead of reading it out of an error.
    ///
    /// Distinct from the frontier: the frontier is what has been *accounted*,
    /// this is what is *held*. Held ≥ accounted always; a sync walk that has
    /// received rows it has not yet applied sits between the two.
    pub async fn max_held_seq(&self, scope: &str, writer: &WriterId) -> Result<Option<u64>> {
        self.backend.max_scope_writer_seq(scope, writer).await
    }

    /// Whether this replica has **listed** `scope` from its bound nest — the
    /// fact a read of the scope is gated on (`account-client-lifecycle.md`
    /// § The client-side lifecycle → *The first listing*, clause (1)). Until
    /// it holds, the store has only what this device wrote itself, and an
    /// empty answer would be read as the account's value.
    pub async fn listed(&self, scope: &str) -> Result<bool> {
        Ok(self
            .backend
            .meta_get(&crate::physical::listed_key(scope))
            .await?
            .is_some())
    }

    /// Record that this replica has listed `scope` ([`Self::listed`]).
    ///
    /// **The caller's law, which nothing here can check:** record only when a
    /// listing of the scope from the replica's bound nest has run to its end
    /// — never for a peer's or a linked nest's listing, an import off another
    /// rail, or a pass that ended without its listing. Never cleared: the
    /// fact goes only with the store.
    pub async fn record_listed(&self, scope: &str) -> Result<()> {
        self.backend
            .meta_put(&crate::physical::listed_key(scope), b"1")
            .await
    }

    /// `scope`'s **unkeyed** set — the generations under which this
    /// replica's listings from its bound nest left a row unopened because
    /// this device held no key for that generation, each with its
    /// **answered-empty** bit: the escrow holder has answered this replica's
    /// request for that generation with no wrap that opens
    /// (`account-client-lifecycle.md` § The client-side lifecycle → *The
    /// first listing*, clause (5), *The fact*). What the unkeyed hold reads;
    /// empty on a store that has recorded nothing.
    pub async fn unkeyed(&self, scope: &str) -> Result<std::collections::BTreeMap<[u8; 32], bool>> {
        let Some(raw) = self
            .backend
            .meta_get(&crate::physical::unkeyed_key(scope))
            .await?
        else {
            return Ok(std::collections::BTreeMap::new());
        };
        // Fixed-width records: the 32-byte generation id, then one byte for
        // the bit.
        let (records, rest) = raw.as_chunks::<33>();
        if !rest.is_empty() {
            bail!(
                "the unkeyed set of {scope:?} is {} bytes, not a whole number of records",
                raw.len()
            );
        }
        Ok(records
            .iter()
            .map(|record| {
                let mut id = [0u8; 32];
                id.copy_from_slice(&record[..32]);
                (id, record[32] != 0)
            })
            .collect())
    }

    async fn put_unkeyed(
        &self,
        scope: &str,
        set: &std::collections::BTreeMap<[u8; 32], bool>,
    ) -> Result<()> {
        let mut raw = Vec::with_capacity(set.len() * 33);
        for (id, answered_empty) in set {
            raw.extend_from_slice(id);
            raw.push(u8::from(*answered_empty));
        }
        self.backend
            .meta_put(&crate::physical::unkeyed_key(scope), &raw)
            .await
    }

    /// A full listing of `scope` ran to its end and left rows unopened for
    /// want of a key under exactly `generations`: the set becomes them, each
    /// surviving id keeping its answered-empty bit and each new one starting
    /// clear. Writes nothing when the set is unchanged.
    ///
    /// The caller's law is [`Self::record_listed`]'s: a listing of the scope
    /// from the replica's bound nest, run to its end — never a nudge's walk,
    /// which only adds ([`Self::add_unkeyed`]).
    pub async fn replace_unkeyed(
        &self,
        scope: &str,
        generations: &std::collections::BTreeSet<[u8; 32]>,
    ) -> Result<()> {
        let held = self.unkeyed(scope).await?;
        let set: std::collections::BTreeMap<[u8; 32], bool> = generations
            .iter()
            .map(|id| (*id, held.get(id).copied().unwrap_or(false)))
            .collect();
        if set == held {
            return Ok(());
        }
        self.put_unkeyed(scope, &set).await
    }

    /// A walk of `scope` from the bound nest left rows unopened for want of a
    /// key under `generations`: add the ones the set lacks, bit clear.
    pub async fn add_unkeyed(
        &self,
        scope: &str,
        generations: &std::collections::BTreeSet<[u8; 32]>,
    ) -> Result<()> {
        let mut set = self.unkeyed(scope).await?;
        let before = set.len();
        for id in generations {
            set.entry(*id).or_insert(false);
        }
        if set.len() == before {
            return Ok(());
        }
        self.put_unkeyed(scope, &set).await
    }

    /// The escrow holder answered this replica's request for `generation`
    /// with no wrap that opens: set its answered-empty bit, durably. `false`
    /// when `scope`'s set does not hold the generation — a question about a
    /// generation no listing left unopened records nothing.
    pub async fn mark_unkeyed_answered_empty(
        &self,
        scope: &str,
        generation: &[u8; 32],
    ) -> Result<bool> {
        let mut set = self.unkeyed(scope).await?;
        match set.get_mut(generation) {
            None => Ok(false),
            Some(true) => Ok(true),
            Some(bit) => {
                *bit = true;
                self.put_unkeyed(scope, &set).await?;
                Ok(true)
            }
        }
    }

    /// The **let-go** set: the dead generations the user let go on this
    /// replica (`account-data-taxonomy.md` § The generation machinery →
    /// *Fleet-scope reclamation*, clause (3)(j)). The let-go deletes each one's
    /// wraps at the bound nest itself; the secondary leg reads this set beside
    /// the shredded generations to delete them at every linked holder too,
    /// idempotently, run after run. Empty on a store that let nothing go.
    pub async fn let_go(&self) -> Result<std::collections::BTreeSet<[u8; 32]>> {
        let Some(raw) = self.backend.meta_get(crate::physical::LET_GO_KEY).await? else {
            return Ok(std::collections::BTreeSet::new());
        };
        let (records, rest) = raw.as_chunks::<32>();
        if !rest.is_empty() {
            bail!(
                "the let-go set is {} bytes, not a whole number of generation ids",
                raw.len()
            );
        }
        Ok(records.iter().copied().collect())
    }

    /// Add `generations` to the let-go set ([`Self::let_go`]), durably.
    /// Writes nothing when every one is already held.
    pub async fn add_let_go(
        &self,
        generations: &std::collections::BTreeSet<[u8; 32]>,
    ) -> Result<()> {
        let mut set = self.let_go().await?;
        let before = set.len();
        set.extend(generations.iter().copied());
        if set.len() == before {
            return Ok(());
        }
        let raw: Vec<u8> = set.iter().flatten().copied().collect();
        self.backend
            .meta_put(crate::physical::LET_GO_KEY, &raw)
            .await
    }

    /// `writer`'s **parked** rows on `scope`: the `writer_seq`s of its own
    /// rows the bound nest refused for room, which the publish owes by name
    /// (`account-replica-posture.md` § The store device principal,
    /// refinement 11 → *A row refused for room is parked*). Empty on a store
    /// that has parked nothing.
    pub async fn parked(
        &self,
        scope: &str,
        writer: &WriterId,
    ) -> Result<std::collections::BTreeSet<u64>> {
        let Some(raw) = self
            .backend
            .meta_get(&crate::physical::parked_key(scope, writer))
            .await?
        else {
            return Ok(std::collections::BTreeSet::new());
        };
        // Fixed-width records: one big-endian `writer_seq` each.
        let (records, rest) = raw.as_chunks::<8>();
        if !rest.is_empty() {
            bail!(
                "the parked list of {scope:?} is {} bytes, not a whole number of records",
                raw.len()
            );
        }
        Ok(records.iter().map(|r| u64::from_be_bytes(*r)).collect())
    }

    async fn put_parked(
        &self,
        scope: &str,
        writer: &WriterId,
        set: &std::collections::BTreeSet<u64>,
    ) -> Result<()> {
        let key = crate::physical::parked_key(scope, writer);
        if set.is_empty() {
            return self.backend.meta_delete(&key).await;
        }
        let raw: Vec<u8> = set.iter().flat_map(|seq| seq.to_be_bytes()).collect();
        self.backend.meta_put(&key, &raw).await
    }

    /// Park `writer`'s row at `writer_seq` on `scope` ([`Self::parked`]).
    /// Idempotent.
    pub async fn park(&self, scope: &str, writer: &WriterId, writer_seq: u64) -> Result<()> {
        let mut set = self.parked(scope, writer).await?;
        if set.insert(writer_seq) {
            self.put_parked(scope, writer, &set).await?;
        }
        Ok(())
    }

    /// Take `writer`'s row at `writer_seq` off `scope`'s parked list.
    /// Returns whether it was parked.
    pub async fn unpark(&self, scope: &str, writer: &WriterId, writer_seq: u64) -> Result<bool> {
        let mut set = self.parked(scope, writer).await?;
        if !set.remove(&writer_seq) {
            return Ok(false);
        }
        self.put_parked(scope, writer, &set).await?;
        Ok(true)
    }

    /// Empty `writer`'s parked list on `scope` — the tail re-author's, once
    /// the rows it names are re-journaled under the successor.
    pub async fn clear_parked(&self, scope: &str, writer: &WriterId) -> Result<()> {
        self.backend
            .meta_delete(&crate::physical::parked_key(scope, writer))
            .await
    }

    /// `scope`'s serve-order watermark ([`StoreBackend::nest_watermark`]): the
    /// nest-log `seq` through which this store holds every row the scope's
    /// sequencing nest serves from a writer a request does not name (charter
    /// § Feeds and cursors → *Compaction is a serve-order watermark*).
    pub async fn nest_watermark(&self, scope: &str) -> Result<Option<u64>> {
        self.backend.nest_watermark(scope).await
    }

    /// The replica id `scope`'s watermark was banked against — the
    /// `replica_id` the sequencing nest named on the echo that banked it
    /// (`account-sync-plane.md` § The bind leg, ruling 2). `None` when no
    /// watermark is banked, or when the nest that banked it named none.
    pub async fn nest_watermark_replica(&self, scope: &str) -> Result<Option<Vec<u8>>> {
        if self.backend.nest_watermark(scope).await?.is_none() {
            return Ok(None);
        }
        self.backend
            .meta_get(&crate::physical::nest_watermark_replica_key(scope))
            .await
    }

    /// Raise `scope`'s watermark to `seq` — never lowering it — and return the
    /// result, keyed by `replica`, the replica id the echo's nest named.
    ///
    /// **A watermark is valid only for the replica it was banked from**
    /// (`account-sync-plane.md` § The bind leg, ruling 2): a bank recorded
    /// against another replica — or against a nest that named none, when this
    /// one names one, or the reverse — is cleared before the raise, so the
    /// max-merge can never keep a stale replica's higher value. The writes are
    /// ordered so a crash between any two leaves no watermark under the wrong
    /// key: clear, re-key, raise.
    ///
    /// **The caller's law, which nothing here can check:** raise only from a
    /// `complete_through_seq` the scope's sequencing nest echoed, and only once
    /// every row of the page it covers has been taken — accounted, or held
    /// verbatim in the relay plane with its writer left for the walk to keep
    /// naming. The store records no nest `seq` to hold that against (a journal
    /// row carries its writer's coordinate, not the nest's), which is why
    /// [`Self::advance_frontier`]'s accounting check has no counterpart here.
    pub async fn raise_nest_watermark(
        &self,
        scope: &str,
        replica: Option<&[u8]>,
        seq: u64,
    ) -> Result<u64> {
        let key = crate::physical::nest_watermark_replica_key(scope);
        let banked = self.backend.meta_get(&key).await?;
        if banked.as_deref() != replica {
            self.backend.nest_watermark_clear(scope).await?;
            match replica {
                Some(id) => self.backend.meta_put(&key, id).await?,
                None => self.backend.meta_delete(&key).await?,
            }
        }
        self.backend.nest_watermark_raise(scope, seq).await
    }

    /// Clear `scope`'s watermark outright — see
    /// [`StoreBackend::nest_watermark_clear`]. Called when a watermark-bearing
    /// walk found the bank void: its nest stopped echoing, named another
    /// replica or none, or echoed below it — never on an ordinary walk.
    pub async fn clear_nest_watermark(&self, scope: &str) -> Result<()> {
        self.backend.nest_watermark_clear(scope).await?;
        self.backend
            .meta_delete(&crate::physical::nest_watermark_replica_key(scope))
            .await
    }

    /// Void `scope`'s watermark: clear it, the replica it was keyed to, and
    /// the feed coordinate of every relay row of the scope
    /// (`account-sync-plane.md` § The bind leg, ruling 2). A coordinate is a
    /// position in the log of the replica the watermark was banked from; kept
    /// across a change of replica, it would withhold a retire against the new
    /// nest's gate by the old nest's numbering.
    ///
    /// **The caller's law:** void before recording the first row of the new
    /// replica, never at the bank — the walk stamps coordinates as it applies
    /// each page, so a void placed after a page would wipe the stamps it just
    /// wrote. The coordinates go first, so a crash between the writes leaves
    /// the watermark standing and the next walk voids again.
    pub async fn void_nest_watermark(&self, scope: &str) -> Result<()> {
        self.backend.relay_clear_feed_seqs(scope).await?;
        self.clear_nest_watermark(scope).await
    }

    /// The **settled replica** (`account-sync-plane.md` § The bind leg, ruling
    /// 2): the bytes the plane recorded for the nest this store last
    /// completed a pass against — the pinned identity and the replica id, in
    /// the plane's own encoding; opaque here. `None` before any pass settled.
    pub async fn settled_replica(&self) -> Result<Option<Vec<u8>>> {
        self.backend
            .meta_get(crate::physical::SETTLED_REPLICA_KEY)
            .await
    }

    /// Record the settled replica — written only when a pass against it
    /// completed, so a verification cut short runs again.
    pub async fn record_settled_replica(&self, value: &[u8]) -> Result<()> {
        self.backend
            .meta_put(crate::physical::SETTLED_REPLICA_KEY, value)
            .await
    }

    /// Current entry for `(kind, key)`.
    pub async fn state(&self, kind: &str, key: &str) -> Result<Option<StateEntry>> {
        self.backend.state_get(kind, key).await
    }

    // ── The group plane's entry table ─────────────────────────────────────────

    /// Write a group-plane class-2 entry — [`Self::put_state`]'s contract on
    /// the `group_entries` table: the entry upsert and its journal row land in
    /// one backend transaction, the store assigns `entry_version` and the
    /// journal seq. `entry.scope` names the group scope
    /// (`fauna_protocol::scope::GroupScope`'s string form), which is also the
    /// journal scope the row lands under. Returns `(entry_version, seq)`.
    pub async fn put_group_state(&self, mut entry: StateEntry) -> Result<(u64, u64)> {
        for _ in 0..APPEND_RETRY_LIMIT {
            let held = self
                .backend
                .group_state_get(&entry.scope, &entry.kind, &entry.key)
                .await?;
            let next_version = held.as_ref().map_or(1, |e| e.entry_version + 1);
            entry.entry_version = next_version;
            let seq = self.next_local_seq().await?;
            let row = JournalRow {
                writer: self.writer,
                seq,
                scope: entry.scope.clone(),
                op: JournalOp::StatePut,
                item: ItemRef::StateKey {
                    kind: entry.kind.clone(),
                    key: entry.key.clone(),
                    entry_version: next_version,
                },
            };
            match self
                .backend
                .group_state_put_with_row(&entry, &row, Some(&self.writer))
                .await?
            {
                InsertOutcome::Inserted => {
                    self.note_entry_write(held.as_ref(), &entry);
                    return Ok((next_version, seq));
                }
                InsertOutcome::OccupiedByDifferent
                | InsertOutcome::IdenticalPresent
                | InsertOutcome::EntryMoved => continue,
            }
        }
        bail!(
            "group state put lost the writer-seq race {APPEND_RETRY_LIMIT} times \
             (writer {})",
            self.writer.to_hex()
        )
    }

    /// Apply another writer's group-plane entry — [`Self::ingest_state`]'s
    /// contract on the `group_entries` table: the origin's journal row and
    /// this replica's merged current value for `(scope, kind, key)` land in
    /// one transaction, idempotent on byte-identical replay, loud on
    /// equivocation.
    pub async fn ingest_group_state(
        &self,
        row: &JournalRow,
        mut entry: StateEntry,
    ) -> Result<InsertOutcome> {
        let mut outcome = InsertOutcome::EntryMoved;
        for _ in 0..APPEND_RETRY_LIMIT {
            let held = self
                .backend
                .group_state_get(&entry.scope, &entry.kind, &entry.key)
                .await?;
            entry.entry_version = held.as_ref().map_or(1, |e| e.entry_version + 1);
            outcome = self
                .backend
                .group_state_put_with_row(&entry, row, None)
                .await?;
            if outcome == InsertOutcome::Inserted {
                self.note_entry_write(held.as_ref(), &entry);
            }
            if outcome != InsertOutcome::EntryMoved {
                break;
            }
        }
        match outcome {
            InsertOutcome::EntryMoved => bail!(
                "group state ingest lost the entry-version race {APPEND_RETRY_LIMIT} times \
                 (scope {:?} kind {:?} key {:?})",
                entry.scope,
                entry.kind,
                entry.key
            ),
            InsertOutcome::OccupiedByDifferent => bail!(
                "journal equivocation: scope {:?} writer {} seq {} already held with \
                 different content — refusing to overwrite (incoming: op {:?} item {:?})",
                row.scope,
                row.writer.to_hex(),
                row.seq,
                row.op.as_str(),
                row.item
            ),
            ok => Ok(ok),
        }
    }

    /// Current group-plane entry for `(scope, kind, key)`.
    pub async fn group_state(
        &self,
        scope: &str,
        kind: &str,
        key: &str,
    ) -> Result<Option<StateEntry>> {
        self.backend.group_state_get(scope, kind, key).await
    }

    /// Every current group-plane entry in `scope`, ordered `(kind, key)` —
    /// what a listing surface renders and a resolver walks.
    pub async fn group_scope_states(&self, scope: &str) -> Result<Vec<StateEntry>> {
        self.backend.group_states_for_scope(scope).await
    }

    /// The backend's cross-connection change counter — the W5.2 notification
    /// floor ([`StoreBackend::data_version`] owns the contract: moves iff
    /// another connection committed; `None` = the medium has no counter).
    pub async fn data_version(&self) -> Result<Option<u64>> {
        self.backend.data_version().await
    }

    /// `writer`'s rows in `scope` after `after`, ascending.
    pub async fn scope_rows(
        &self,
        scope: &str,
        writer: &WriterId,
        after: u64,
        limit: u32,
    ) -> Result<Vec<JournalRow>> {
        self.backend
            .rows_for_scope(scope, writer, after, limit)
            .await
    }

    // ── The relay plane (W2.6, charter § The peer leg) ────────────────────────

    /// Record (or supersede) the live relay row for `(scope, writer, item)` —
    /// the verbatim wire row this replica can later serve a peer (see
    /// [`RelayRow`]). Idempotent; a row at or below the held `writer_seq` is
    /// a no-op (replays never regress the live row).
    pub async fn record_relay_row(&self, row: &RelayRow) -> Result<()> {
        self.backend.relay_put(row).await
    }

    /// Retire the relay rows held at `(scope, writer, writer_seq)` under any
    /// item other than `keep_item_key`
    /// ([`StoreBackend::relay_retire_shadowed`]) — the walk's carry arm for a
    /// retired burnt writer (refinement 11): the row that journal recorded
    /// there before a send the nest refused is not the fleet's row at the
    /// coordinate, and a peer must
    /// not be served it as this replica's word. Returns the rows deleted.
    pub async fn retire_shadowed_relay_rows(
        &self,
        scope: &str,
        writer: &WriterId,
        writer_seq: u64,
        keep_item_key: &[u8],
    ) -> Result<u64> {
        self.backend
            .relay_retire_shadowed(scope, writer, writer_seq, keep_item_key)
            .await
    }

    /// Retire every relay row held at `(scope, writer, writer_seq)`
    /// ([`StoreBackend::relay_retire_at`]) — the publish leg's answer to the
    /// nest's final refusal of that coordinate (`stale_writer_seq`),
    /// refinement 11's *a refused row's
    /// relay residue*: the relay plane serves what the nest holds or will
    /// hold, and a row it has refused for good is neither. Returns the rows
    /// deleted.
    pub async fn retire_relay_rows_at(
        &self,
        scope: &str,
        writer: &WriterId,
        writer_seq: u64,
    ) -> Result<u64> {
        self.backend
            .relay_retire_at(scope, writer, writer_seq)
            .await
    }

    /// Live relay rows in `scope` of `item_class` past `frontier`, ordered
    /// `(writer, writer_seq)` ascending, at most `limit` — what a peer's
    /// accounted walk pages (the nest feed's exact serve semantics: one live
    /// row per `(item, writer)`, ordered prefixes per writer).
    pub async fn relay_rows(
        &self,
        scope: &str,
        item_class: &str,
        frontier: &[(WriterId, u64)],
        limit: u32,
    ) -> Result<Vec<RelayRow>> {
        self.backend
            .relay_rows(scope, item_class, frontier, limit)
            .await
    }

    /// Forget a merged state entry locally — no journal row, nothing
    /// published ([`StoreBackend::state_forget`]): the reclamation pass's
    /// hygiene for a row that is dead everywhere.
    pub async fn forget_state(&self, kind: &str, key: &str) -> Result<()> {
        let held = self.backend.state_get(kind, key).await?.is_some();
        self.backend.state_forget(kind, key).await?;
        if held {
            self.entry_changed();
        }
        Ok(())
    }

    /// Drop a live relay row locally ([`StoreBackend::relay_forget`]).
    pub async fn relay_forget(
        &self,
        scope: &str,
        writer: &WriterId,
        item_key: &[u8],
    ) -> Result<()> {
        self.backend.relay_forget(scope, writer, item_key).await
    }

    /// The generations `scope`'s live relay rows are sealed under
    /// ([`StoreBackend::relay_generations`]).
    pub async fn relay_generations(&self, scope: &str) -> Result<Vec<[u8; 32]>> {
        self.backend.relay_generations(scope).await
    }

    /// Drop every relay row in `scope` sealed under `generation`, locally
    /// ([`StoreBackend::relay_forget_sealed_under`]).
    pub async fn relay_forget_sealed_under(
        &self,
        scope: &str,
        generation: &[u8; 32],
    ) -> Result<u64> {
        self.backend
            .relay_forget_sealed_under(scope, generation)
            .await
    }

    /// Every live relay row in `scope` sealed under `generation`
    /// ([`StoreBackend::relay_rows_sealed_under`]).
    pub async fn relay_rows_sealed_under(
        &self,
        scope: &str,
        generation: &[u8; 32],
    ) -> Result<Vec<RelayRow>> {
        self.backend
            .relay_rows_sealed_under(scope, generation)
            .await
    }

    /// The live relay rows at one blinded item key, one per writer
    /// ([`StoreBackend::relay_rows_at`]).
    pub async fn relay_rows_at(
        &self,
        scope: &str,
        item_class: &str,
        item_key: &[u8],
    ) -> Result<Vec<RelayRow>> {
        self.backend
            .relay_rows_at(scope, item_class, item_key)
            .await
    }

    /// One writer's live relay rows in `scope`, ascending
    /// ([`StoreBackend::relay_rows_of_writer`]).
    pub async fn relay_rows_of_writer(
        &self,
        scope: &str,
        item_class: &str,
        writer: &WriterId,
    ) -> Result<Vec<RelayRow>> {
        self.backend
            .relay_rows_of_writer(scope, item_class, writer)
            .await
    }

    /// Stamp a live relay row with its feed coordinate
    /// ([`StoreBackend::relay_stamp_feed_seq`]) — what the publish leg
    /// records once the nest's put reply names the `seq` it assigned.
    pub async fn stamp_relay_feed_seq(
        &self,
        scope: &str,
        writer: &WriterId,
        item_key: &[u8],
        writer_seq: u64,
        feed_seq: u64,
    ) -> Result<()> {
        self.backend
            .relay_stamp_feed_seq(scope, writer, item_key, writer_seq, feed_seq)
            .await
    }

    /// Record one retire a bound plane sent in the **retire record**
    /// (`account-sync-plane.md` § The bind leg, ruling 5) — by whichever
    /// process sent it. Keyed by the retire's coordinates and bounded by
    /// [`ISSUED_RETIRES_CAP`], newest kept
    /// ([`StoreBackend::issued_retire_put`]).
    pub async fn record_issued_retire(&self, retire: &IssuedRetire) -> Result<()> {
        self.backend
            .issued_retire_put(retire, ISSUED_RETIRES_CAP)
            .await
    }

    /// The retire record, oldest first, each entry with its order token
    /// ([`StoreBackend::issued_retires`]) — what the secondary leg re-issues
    /// at each linked nest.
    pub async fn issued_retires(&self) -> Result<Vec<(u64, IssuedRetire)>> {
        self.backend.issued_retires().await
    }

    /// Clear the retire record through the order token `through` — what a
    /// leg run that read up to it clears when it ends; an entry recorded
    /// since stays for the next run.
    pub async fn clear_issued_retires_through(&self, through: u64) -> Result<()> {
        self.backend.issued_retires_clear_through(through).await
    }

    /// Per writer, the highest `writer_seq` this store's relay plane holds in
    /// `scope` for `item_class` ([`StoreBackend::relay_high_waters`]) — what
    /// it has SEEN of each writer, where [`Self::frontier`] is what it has
    /// ACCOUNTED.
    pub async fn relay_high_waters(
        &self,
        scope: &str,
        item_class: &str,
    ) -> Result<Vec<(WriterId, u64)>> {
        self.backend.relay_high_waters(scope, item_class).await
    }

    // ── Custody metering + payload eviction (T15) ────────────────────────────
    //
    // Owner: `docs/goal/architecture/account-data-plane.md` § Replica posture →
    // *Custody policy*. The arithmetic over these numbers is
    // `fauna_core::custody_policy`, deliberately pure so the host-side facet
    // renders exactly what the runtime enforces.

    /// Both planes this store retains, metered per scope family, as the shape
    /// [`plan_custody_eviction`](fauna_core::custody_policy::plan_custody_eviction)
    /// takes: the relay rows (one family per `(scope, item_class)`) and the
    /// adopted segment files (one [`SEGMENT_ITEM_CLASS`] family per scope —
    /// `.dat` evictable, `.meta` floor). `floor_ops` names the relay ops whose
    /// payload is always-present floor (T15 names tombstones); the caller owns
    /// those wire strings.
    ///
    /// Both planes, because T15 bounds *retained bytes*: a custodian adopting
    /// the owner's segments (the bootstrap contract's bulk half) holds them as
    /// surely as it holds relay rows, and a meter blind to them lets the
    /// segment volume overrun `retained_bytes_cap` with every control — the
    /// plan, the ingest brake, the host's `held_bytes` — reading it as empty.
    pub async fn custody_meter(&self, floor_ops: &[&str]) -> Result<CustodyMeter> {
        let mut scopes: Vec<ScopeMeter> = self
            .backend
            .relay_meter(floor_ops)
            .await?
            .into_iter()
            .map(|m| ScopeMeter {
                scope: m.scope,
                item_class: m.item_class,
                rows: m.rows,
                payload_bytes: m.payload_bytes,
                evictable_rows: m.evictable_rows,
                evictable_bytes: m.evictable_bytes,
            })
            .collect();
        scopes.extend(
            self.backend
                .segment_meter()
                .await?
                .into_iter()
                .map(|m| ScopeMeter {
                    scope: m.scope,
                    item_class: SEGMENT_ITEM_CLASS.to_string(),
                    rows: m.segments,
                    payload_bytes: m.dat_bytes.saturating_add(m.meta_bytes),
                    evictable_rows: m.held_segments,
                    evictable_bytes: m.dat_bytes,
                }),
        );
        Ok(CustodyMeter { scopes })
    }

    /// Run an eviction plan: free each family's payload target, oldest first,
    /// floor untouched. Reports what was actually freed across the whole plan
    /// — the number that must reach a receipt, because T15's eviction is
    /// receipt-visible or it is silent data loss.
    pub async fn apply_custody_eviction(
        &self,
        plan: &CustodyEvictionPlan,
        floor_ops: &[&str],
    ) -> Result<RelayEvicted> {
        let mut total = RelayEvicted::default();
        for step in &plan.evictions {
            let freed = if step.item_class == SEGMENT_ITEM_CLASS {
                self.backend
                    .segment_evict_dat(&step.scope, step.target_bytes)
                    .await?
            } else {
                self.backend
                    .relay_evict_payload(
                        &step.scope,
                        &step.item_class,
                        step.target_bytes,
                        floor_ops,
                    )
                    .await?
            };
            total.rows += freed.rows;
            total.bytes = total.bytes.saturating_add(freed.bytes);
        }
        Ok(total)
    }

    /// Every live (non-tombstone) state entry of `kind`, ordered by key —
    /// per-kind enumeration for consumers whose kinds keep one entry per
    /// logical key (discovery reads `fauna.state.device-endpoints`, one entry
    /// per device writer).
    pub async fn states_of_kind(&self, kind: &str) -> Result<Vec<StateEntry>> {
        self.backend.state_entries_of_kind(kind).await
    }

    // ── The block plane (charter § Store logical schema component 1) ─────────

    /// This replica's hydration policy, from store meta. Absent = the
    /// conservative [`HydrationPolicy::default`] (hydrate everything).
    pub async fn hydration_policy(&self) -> Result<HydrationPolicy> {
        match self.backend.meta_get(META_HYDRATION_POLICY).await? {
            Some(bytes) => HydrationPolicy::decode(&bytes),
            None => Ok(HydrationPolicy::default()),
        }
    }

    /// Set this replica's hydration policy. Purely local: policy is a device's
    /// storage preference and never travels the plane.
    pub async fn set_hydration_policy(&self, policy: &HydrationPolicy) -> Result<()> {
        self.backend
            .meta_put(META_HYDRATION_POLICY, &policy.encode())
            .await
    }

    /// Stage a locally-authored class-1 record: its bytes become a loose
    /// block, its index row lands, and a `record-added` row is journaled on the
    /// local writer's log — all in one transaction. Returns
    /// `(cid, seq)`.
    ///
    /// The CID is **computed here from the bytes**, never accepted from the
    /// caller: `record_id == block CID` is an identity, so letting a caller
    /// name one would let it name a record whose bytes say otherwise.
    ///
    /// It is computed **dag-cbor-coded**, matching how every segment record's
    /// CID is computed (`message-segment-store.md` § Segment file format;
    /// `bins/fauna-nest/src/segments/post.rs` — `Cid::of_dag_cbor(body)`,
    /// whose digest *is* the shipped `post_id`). The codec byte is part of the
    /// 36-byte identity, so a raw-coded CID over the same bytes is a
    /// *different* key: had this path used one, a record staged here and then
    /// folded into a nest segment would index twice under two identities, and
    /// the replica would never recognise the segment copy as the record it
    /// authored itself.
    ///
    /// Locally-authored records never wait for a segment (charter: they "stage
    /// as loose blocks, journal on the device's own log, and publish by
    /// own-log replay on reconnect" — the W4 phase-0 ruling: an `OfflineSafe`
    /// store write never enters the outbox; its journal row is its replay
    /// record).
    /// Hydration policy deliberately does **not** gate this: a replica always
    /// holds bytes it authored itself until something else has them.
    pub async fn stage_local_record(
        &self,
        scope: &str,
        kind: &str,
        bytes: &[u8],
    ) -> Result<(ContentHash, u64)> {
        let cid = ContentHash::of_dag_cbor(bytes);
        let entry = RecordIndexEntry {
            cid,
            scope: scope.to_string(),
            kind: kind.to_string(),
            size: Some(bytes.len() as u64),
        };
        for _ in 0..APPEND_RETRY_LIMIT {
            let seq = self.next_local_seq().await?;
            let row = JournalRow {
                writer: self.writer,
                seq,
                scope: scope.to_string(),
                op: JournalOp::RecordAdded,
                item: ItemRef::Cid(cid),
            };
            match self
                .backend
                .record_added_with_row(&entry, Some(bytes), &row, Some(&self.writer))
                .await?
            {
                InsertOutcome::Inserted => {
                    self.entry_changed();
                    return Ok((cid, seq));
                }
                InsertOutcome::OccupiedByDifferent
                | InsertOutcome::IdenticalPresent
                | InsertOutcome::EntryMoved => continue,
            }
        }
        bail!(
            "record staging lost the writer-seq race {APPEND_RETRY_LIMIT} times \
             (writer {})",
            self.writer.to_hex()
        )
    }

    /// Record that a class-1 record exists without holding its bytes — the
    /// index-only half a walk produces from another writer's `record-added`
    /// row. Idempotent.
    ///
    /// This is what "the always-present layer is the index, not the blocks"
    /// means operationally: every record the replica *knows of* is indexed
    /// immediately; whether its bytes ever arrive is hydration policy's call.
    pub async fn note_record(&self, entry: &RecordIndexEntry) -> Result<()> {
        let held = self.backend.record_index_get(&entry.cid).await?;
        self.backend.record_index_put(entry).await?;
        if held.as_ref() != Some(entry) {
            self.entry_changed();
        }
        Ok(())
    }

    /// Apply a class-1 tombstone: the record's index row goes, and its **loose**
    /// bytes with it. Returns whether this replica knew the record at all — a
    /// tombstone for a record never seen is a no-op, not an error, because a
    /// zero-frontier reconcile re-presents every live tombstone on a scope and a
    /// replica that bootstrapped after the delete legitimately never held it.
    ///
    /// Idempotent by construction (a second apply finds no row), which is what
    /// lets the walk replay a page without special-casing.
    ///
    /// **Segment-resident bytes survive**, exactly as [`Self::dehydrate`]
    /// explains: a CARv2 segment is immutable, there is no per-block hole to
    /// punch, and reclaiming the space is segment eviction — a whole-file
    /// operation. That is not a leak of a deleted record: the index is the
    /// always-present layer, so with its row gone the record is reachable
    /// through no store API and no read path can surface it. The nest's own
    /// deletion order is the same shape (`routes.rs::delete_post_core` removes
    /// the projection first and leaves the segment body for compaction to
    /// reclaim).
    pub async fn apply_tombstone(&self, cid: &ContentHash) -> Result<bool> {
        let known = self.backend.record_index_delete(cid).await?;
        self.backend.block_delete(cid).await?;
        if known {
            self.entry_changed();
        }
        Ok(known)
    }

    /// Supply a known record's bytes, verifying them against its CID.
    ///
    /// Refuses bytes for a record with no index row: a block whose record the
    /// replica cannot describe is unreachable through every store API, so
    /// accepting it would only grow the DB.
    pub async fn hydrate(&self, cid: &ContentHash, bytes: &[u8]) -> Result<()> {
        self.put_block(cid, bytes).await?;
        if self.backend.record_index_get(cid).await?.is_none() {
            bail!(
                "hydrate: no record-index row for {} — index the record before its bytes",
                hex_full(cid.as_bytes())
            );
        }
        Ok(())
    }

    /// Store block bytes under a CID, **verifying the content address**.
    ///
    /// The verification is the whole point of a content-addressed plane and is
    /// cheap: without it a peer or a corrupted segment could file arbitrary
    /// bytes under a well-known CID and every later reader would trust them.
    /// Same predicate as the nest chunk route's F9 anti-poisoning check.
    ///
    /// The check is on the **digest**, so it is codec-agnostic: the plane
    /// carries dag-cbor-coded record CIDs (see [`Self::stage_local_record`])
    /// beside raw-coded blob CIDs (media, imported files — `fauna_cbor::Cid`'s
    /// codec guidance), and the digest is the whole of what protects against
    /// poisoning. The codec byte is part of the key, not of the proof: bytes
    /// offered under the wrong codec land under a different key, they do not
    /// pass as another record's.
    pub async fn put_block(&self, cid: &ContentHash, bytes: &[u8]) -> Result<()> {
        if !cid.matches(bytes) {
            bail!(
                "block content-address mismatch: bytes hash to {} but were offered as {}",
                hex_full(ContentHash::of_dag_cbor(bytes).as_bytes()),
                hex_full(cid.as_bytes())
            );
        }
        self.backend.block_put(cid, bytes).await
    }

    /// A record's bytes, if held.
    pub async fn block(&self, cid: &ContentHash) -> Result<Option<Vec<u8>>> {
        self.backend.block_get(cid).await
    }

    /// Whether this replica holds `cid`'s bytes. Derived from the block plane,
    /// never a stored flag — an index row and its bytes cannot disagree.
    pub async fn is_present(&self, cid: &ContentHash) -> Result<bool> {
        self.backend.block_has(cid).await
    }

    /// A record's index row, held bytes or not.
    pub async fn record(&self, cid: &ContentHash) -> Result<Option<RecordIndexEntry>> {
        self.backend.record_index_get(cid).await
    }

    /// A scope's index rows in stable CID order, resumable via `after`.
    pub async fn records_in_scope(
        &self,
        scope: &str,
        after: Option<&ContentHash>,
        limit: u32,
    ) -> Result<Vec<RecordIndexEntry>> {
        self.backend.records_in_scope(scope, after, limit).await
    }

    /// Drop a record's bytes while keeping its index row — the dehydration
    /// half of hydration accounting. Returns whether bytes were held.
    ///
    /// **Refuses a kind the policy hydrates always.** Otherwise "always
    /// hydrated" would be a comment rather than an accounting rule, and a
    /// caller could quietly empty the very kinds the replica promised to serve
    /// offline. Changing the policy first is the supported path.
    /// **Refuses a segment-resident block.** A CARv2 segment is immutable, so
    /// there is no per-block hole to punch in one; reclaiming its space is
    /// segment eviction, a whole-file operation. Returning `true` here would
    /// claim bytes were dropped that [`Self::block`] still serves.
    pub async fn dehydrate(&self, cid: &ContentHash) -> Result<bool> {
        let Some(entry) = self.backend.record_index_get(cid).await? else {
            bail!(
                "dehydrate: no record-index row for {}",
                hex_full(cid.as_bytes())
            );
        };
        if self.hydration_policy().await?.for_kind(&entry.kind) == Hydration::Always {
            bail!(
                "dehydrate refused: kind {:?} is always-hydrated under this replica's policy \
                 — change the policy first",
                entry.kind
            );
        }
        if let Some(key) = self.backend.segment_of_block(cid).await? {
            bail!(
                "dehydrate refused: {} lives in adopted segment {}/{}/{} — a segment is \
                 immutable, so reclaiming it is segment eviction, not per-block dehydration",
                hex_full(cid.as_bytes()),
                key.scope,
                key.kind,
                key.segment_id
            );
        }
        self.backend.block_delete(cid).await
    }

    // ── Segment adoption + bootstrap (charter § the bootstrap contract) ──────

    /// Adopt one pulled segment pair into `scope`, verbatim.
    ///
    /// Verification is [`crate::segments::admit`]'s — the bytes reach the
    /// backend only after the container has proved it carries exactly the
    /// records its sidecar declares, each hashing to the CID it is filed
    /// under, for this actor. Adoption then indexes every record it carries,
    /// because an adopted block whose record has no index row is reachable
    /// through no store API.
    ///
    /// Idempotent: re-adopting a held `(scope, kind, segment_id)` reports
    /// `None` and writes nothing. `Some(n)` = newly adopted, `n` records
    /// indexed.
    ///
    /// This does **not** journal. A segment is bulk transport of class-1 truth
    /// the origin already journaled; re-announcing its records on this
    /// replica's own log would forge authorship of records it merely mirrors.
    /// The feed walk (W2.4) is what reconciles the journal.
    ///
    /// The pair takes the one adoption path every source takes: staged in the
    /// backend's segment area, admitted by reading it back, adopted by rename
    /// ([`Self::bootstrap_scope_segments`] stages a transfer as it arrives;
    /// this stages bytes already in hand).
    pub async fn adopt_segment(
        &self,
        scope: &str,
        dat: &[u8],
        meta: &[u8],
    ) -> Result<Option<usize>> {
        let mut staged = self.backend.segment_stage().await?;
        staged.write(SegmentHalf::Dat, dat).await?;
        staged.write(SegmentHalf::Meta, meta).await?;
        self.adopt_staged(scope, staged).await
    }

    /// [`Self::adopt_segment`] over a pair already staged.
    async fn adopt_staged(&self, scope: &str, mut staged: B::Staging) -> Result<Option<usize>> {
        // The pair must belong to the scope it is being filed under: for a
        // content scope the expected key is the scope's own id — the owner for
        // single-principal kinds (== this store's actor on every path that
        // existed before conv), the CHANNEL for a co-authored kind (`conv`),
        // whose sidecar names the channel and never this store's actor. A
        // non-content scope falls back to the store actor and has no kind to
        // bind.
        let (expect_kind, expect_actor) = match content_scope_key(scope) {
            Some((kind, id)) => (Some(kind), id),
            None => (
                None,
                fauna_core::hex32::decode(&self.actor_id_hex)
                    .context("this store's actor id is not 32 hex-encoded bytes")?,
            ),
        };
        let meta = staged.meta().await?;
        let SegmentAdmission { sidecar, blocks } = admit(
            staged.dat_reader().await?,
            &meta,
            &expect_actor,
            expect_kind,
        )?;
        let key = SegmentKey {
            scope: scope.to_string(),
            kind: sidecar.kind.clone(),
            segment_id: sidecar.segment_id,
        };
        if !self.backend.segment_adopt(&key, staged, &blocks).await? {
            return Ok(None);
        }
        for block in &blocks {
            self.backend
                .record_index_put(&RecordIndexEntry {
                    cid: block.cid,
                    scope: scope.to_string(),
                    kind: sidecar.kind.clone(),
                    size: Some(block.len),
                })
                .await?;
        }
        if !blocks.is_empty() {
            self.entry_changed();
        }
        Ok(Some(blocks.len()))
    }

    /// The segments this replica holds for `scope`.
    pub async fn adopted_segments(&self, scope: &str) -> Result<Vec<SegmentKey>> {
        self.backend.segments_in_scope(scope).await
    }

    /// **Scope departure** — T2 transition 3, charter § The replica boundary:
    /// "leaving a channel, a shared set unshared, membership revoked … drops
    /// that scope's items from the replica — the data belonged to the
    /// membership."
    ///
    /// This is the store half: given a scope the caller has already concluded
    /// is departed, remove it from every plane. **Deciding** a scope departed
    /// is emphatically not this layer's job and must not be inferred from the
    /// store's contents — `fauna_sync_engine::departure` owns that judgment
    /// and the refusals that keep a still-loading membership source from
    /// reading as "left everything".
    ///
    /// Three properties worth stating where the call sites can see them:
    ///
    /// - **The scope's seen-set entry survives**, and structurally, not by
    ///   care: the entry lives in the *account-state* scope and merely carries
    ///   the departed scope's string as its **key**, so a `WHERE scope = ?`
    ///   delete cannot reach it. That is the shape T2 transition 4 requires —
    ///   the seen-set is grow-only as a set, "membership never shrinks" is the
    ///   merge law, and shrinking one replica's entry would make the fleet's
    ///   join non-monotonic. The account observed those items; leaving the
    ///   channel does not unobserve them.
    /// - **A sibling scope's records cannot be collateral.** `record_index` is
    ///   keyed by CID alone, so a record belongs to exactly one scope in it,
    ///   and the loose blocks removed here are only those of rows this drop
    ///   deleted.
    /// - **A re-join re-bootstraps.** The frontier row goes with everything
    ///   else, so re-deriving the scope later starts its walk at cursor zero —
    ///   the same path a fresh replica takes, with no special re-join code.
    pub async fn drop_scope(&self, scope: &str) -> Result<ScopeDropCounts> {
        let dropped = self.backend.drop_scope(scope).await?;
        if !dropped.is_empty() {
            self.entry_changed();
        }
        Ok(dropped)
    }

    // ── The outbox (W4, charter § The offline-mutation contract) ─────────────

    /// Append a durable intent (the phase-0 ruling: `OfflineQueued` only —
    /// enforced at the engine's enqueue door, which holds the protocol dep
    /// this crate deliberately lacks). Idempotent on `intent_id`: a re-append
    /// is the composer's crash-retry and reports `false`.
    ///
    /// An undrained intent is the only copy of the user's pending write. The
    /// outbox is outside every scope-keyed plane, so [`Self::drop_scope`]
    /// structurally cannot reach it — a departed channel's intents survive
    /// and park at drain instead.
    pub async fn outbox_append(&self, intent: &NewOutboxIntent) -> Result<bool> {
        self.backend.outbox_append(intent, Some(&self.writer)).await
    }

    /// Every undrained intent, `(scope, channel_seq)` ascending. Drain policy
    /// (parked scopes, the drainer split, backoff) is
    /// `fauna_sync_engine::outbox`'s.
    pub async fn outbox_undrained(&self) -> Result<Vec<OutboxIntent>> {
        self.backend.outbox_undrained().await
    }

    /// Completion-is-deletion. `false` = already drained (a crash-replayed
    /// drain's double-ack, a no-op).
    pub async fn outbox_ack(&self, intent_id: &[u8; 16]) -> Result<bool> {
        self.backend.outbox_ack(intent_id).await
    }

    /// Park the intent as permanently failed — user-visible, never silently
    /// dropped, never re-attempted without user action.
    pub async fn outbox_mark_failed(&self, intent_id: &[u8; 16]) -> Result<bool> {
        self.backend.outbox_mark_failed(intent_id).await
    }

    /// Record an inconclusive drain attempt (the backoff pair).
    pub async fn outbox_record_attempt(&self, intent_id: &[u8; 16]) -> Result<bool> {
        self.backend.outbox_record_attempt(intent_id).await
    }

    /// Rebuild `scope`'s record index from the segments this replica holds —
    /// the mirror-is-rebuildable property (`message-segment-store.md`
    /// § `segment_records` SQLite mirror, applied client-side).
    ///
    /// Walks each segment's sidecar `record_order` in **append order**, which
    /// is why the sidecar exists at all: CARv2's own index is digest-sorted
    /// and cannot reconstruct the order the nest wrote records in. Returns how
    /// many index rows it wrote.
    ///
    /// Safe to run at any time: index rows are upserts of facts the segment
    /// files already carry, so a rebuild converges rather than accumulating.
    pub async fn rebuild_index_from_segments(&self, scope: &str) -> Result<usize> {
        let mut written = 0usize;
        for key in self.backend.segments_in_scope(scope).await? {
            let Some(meta) = self.backend.segment_meta(&key).await? else {
                bail!(
                    "segment {}/{}/{} is listed but holds no sidecar",
                    key.scope,
                    key.kind,
                    key.segment_id
                );
            };
            let sidecar = SegmentSidecarView::decode(&meta)?;
            for cid in &sidecar.record_order {
                self.backend
                    .record_index_put(&RecordIndexEntry {
                        cid: *cid,
                        scope: scope.to_string(),
                        kind: sidecar.kind.clone(),
                        size: self.backend.segment_block_len(cid).await?,
                    })
                    .await?;
                written += 1;
            }
        }
        Ok(written)
    }

    /// Bootstrap `scope`'s class-1 truth from `source` — the **segments** half
    /// of "segments-then-feed-walk per scope".
    ///
    /// For each segment the source offers: a kind the hydration policy keeps
    /// [`Hydration::Always`] is pulled and adopted; a kind it holds
    /// [`Hydration::OnDemand`] is **skipped entirely**, because a dehydrating
    /// replica materialises that scope's index from the feed walk alone and
    /// fetches blocks on demand (charter: "verbatim segment adoption is the
    /// bulk path for scopes the policy hydrates"). Skipping is the point of
    /// the policy: pulling a bulky scope's whole segment set and immediately
    /// dropping the bytes is the work the policy exists to avoid.
    ///
    /// An offer whose `kind` disagrees with the scope it is offered under is
    /// **refused before it is fetched** — trusting the label over the scope
    /// would let a source relabel a bulky dehydrated kind as one the policy
    /// always hydrates and defeat the point of the skip above. A sidecar that
    /// disagrees with its *own* offer is caught after the fetch, by
    /// [`Self::adopt_segment`]'s door check; either refusal is counted, not
    /// propagated, so one mislabelled segment does not abort the rest of the
    /// scope's pull.
    ///
    /// An offer this replica **already holds** — its `(scope, kind,
    /// segment_id)` row is present, `.dat` evicted or not — is counted
    /// `already_held` and never fetched: a finalized segment is immutable, so
    /// re-downloading it only to have adoption discard it is pure waste, and
    /// for a custody-evicted segment it would be a re-download every pass.
    ///
    /// The **feed walk is the caller's next step, not this method's** — it
    /// needs the generalized feed (W2.3's wire, consumed by W2.4's walk), and
    /// runs from a zero frontier for the scope. What this returns is the
    /// report of what the bulk half achieved.
    pub async fn bootstrap_scope_segments<S: BootstrapSource>(
        &self,
        scope: &str,
        source: &S,
    ) -> Result<BootstrapReport> {
        self.bootstrap_segments_inner(scope, source, None).await
    }

    /// [`Self::bootstrap_scope_segments`] under a byte budget — the custody
    /// arm, where `budget` is the headroom the custody's `retained_bytes_cap`
    /// leaves over what the store already holds ([`Self::custody_meter`]).
    ///
    /// The budget is also the download bound (`message-segment-store.md`
    /// § Segment size: no legal ceiling exists — a segment rolls only on its
    /// month bucket — so the bound is the budget, never a guessed constant):
    ///
    /// - an offer whose **declared** `.dat` size exceeds what is left is
    ///   skipped before any byte moves (`skipped_over_budget`);
    /// - every fetch is capped at what is left, so an undeclared or dishonest
    ///   size is refused mid-stream rather than staged whole;
    /// - a fetched pair whose `.dat` + `.meta` together overrun what is left is
    ///   not adopted (`skipped_over_budget`).
    ///
    /// Each adopted pair's bytes are subtracted from `budget`, so one pass
    /// never adopts past the cap: the custodian stops adopting new segments
    /// once full, and eviction ([`Self::apply_custody_eviction`]) is what
    /// frees room when the cap shrinks or the relay plane grows.
    pub async fn bootstrap_scope_segments_within<S: BootstrapSource>(
        &self,
        scope: &str,
        source: &S,
        budget: &mut u64,
    ) -> Result<BootstrapReport> {
        self.bootstrap_segments_inner(scope, source, Some(budget))
            .await
    }

    async fn bootstrap_segments_inner<S: BootstrapSource>(
        &self,
        scope: &str,
        source: &S,
        mut budget: Option<&mut u64>,
    ) -> Result<BootstrapReport> {
        let policy = self.hydration_policy().await?;
        let scope_kind = content_scope_key(scope).map(|(kind, _)| kind.to_string());
        let mut report = BootstrapReport::default();
        for offer in source.list_segments(scope).await? {
            if policy.for_kind(&offer.kind) == Hydration::OnDemand {
                report.skipped_dehydrating += 1;
                continue;
            }
            if scope_kind.as_deref().is_some_and(|kind| kind != offer.kind) {
                report.refused_kind_mismatch += 1;
                continue;
            }
            let key = SegmentKey {
                scope: scope.to_string(),
                kind: offer.kind.clone(),
                segment_id: offer.segment_id,
            };
            if self.backend.segment_meta(&key).await?.is_some() {
                report.already_held += 1;
                continue;
            }
            let max_bytes = budget.as_deref().copied().unwrap_or(u64::MAX);
            if offer.dat_size.is_some_and(|declared| declared > max_bytes) {
                report.skipped_over_budget += 1;
                continue;
            }
            // Staged, not buffered: the transfer writes each chunk to the
            // backend's segment area as it arrives, so it holds one chunk —
            // never a half, whose size only the source chose. A slot dropped
            // un-adopted (an error, a refusal, the budget) takes its bytes
            // with it.
            let mut staged = self.backend.segment_stage().await?;
            source
                .fetch_segment(scope, &offer, max_bytes, &mut staged)
                .await?;
            let pair_bytes = staged
                .len(SegmentHalf::Dat)
                .saturating_add(staged.len(SegmentHalf::Meta));
            if pair_bytes > max_bytes {
                report.skipped_over_budget += 1;
                continue;
            }
            match self.adopt_staged(scope, staged).await {
                Ok(Some(indexed)) => {
                    report.adopted += 1;
                    report.records_indexed += indexed;
                    if let Some(left) = budget.as_deref_mut() {
                        *left = left.saturating_sub(pair_bytes);
                    }
                }
                Ok(None) => report.already_held += 1,
                Err(e) if e.downcast_ref::<SegmentKindMismatch>().is_some() => {
                    report.refused_kind_mismatch += 1;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(report)
    }

    async fn append_local(&self, scope: &str, op: JournalOp, item: ItemRef) -> Result<u64> {
        for _ in 0..APPEND_RETRY_LIMIT {
            let seq = self.next_local_seq().await?;
            let row = JournalRow {
                writer: self.writer,
                seq,
                scope: scope.to_string(),
                op,
                item: item.clone(),
            };
            match self.backend.insert_row(&row, Some(&self.writer)).await? {
                InsertOutcome::Inserted => return Ok(seq),
                InsertOutcome::OccupiedByDifferent
                | InsertOutcome::IdenticalPresent
                | InsertOutcome::EntryMoved => continue,
            }
        }
        bail!(
            "local append lost the writer-seq race {APPEND_RETRY_LIMIT} times \
             (writer {})",
            self.writer.to_hex()
        )
    }

    async fn next_local_seq(&self) -> Result<u64> {
        Ok(self
            .backend
            .max_writer_seq(&self.writer)
            .await?
            .map_or(1, |m| m + 1))
    }
}

/// One segment a bootstrap source is willing to hand over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentOffer {
    /// The segment-store kind tag — the axis hydration policy is read on, so
    /// the offer must carry it or the policy could not skip a bulky scope
    /// without fetching it first.
    pub kind: String,
    pub segment_id: u32,
    /// Byte size of the `.dat`, when the source knows it. Advisory: a caller
    /// budgeting a pull reads it; adoption never trusts it.
    pub dat_size: Option<u64>,
}

/// Where a bootstrap pulls segments from.
///
/// A trait rather than a concrete client because the transport is above this
/// crate in every direction: the nest today over the custodian-pull byte plane
/// (`message-segment-store.md` § Client-device custodian (pull)), a peer at
/// W2.6 over the same contract on the transport seam. The store's floor holds
/// no network dependency and does not gain one here.
///
/// # Not the same seam as `fauna_sync_engine::segment_backup::SegmentSource`
///
/// That one is the **backup** source arm — where a backup pass *reads the
/// owner's segments to ship them elsewhere*. This one is the **adoption**
/// arm — where a fresh replica *gets segments to become a replica*. The names
/// were briefly identical; they are separated because a cold read must not
/// have to work out which is which. Three concrete reasons this is not a
/// second spelling of that trait:
///
/// * It cannot live here. That trait names `fauna_client::NestClient` and
///   `fauna_protocol::SegmentRef` and is `async_trait` + `Send + Sync`; this
///   crate's floor excludes all of it, and none of it compiles for wasm32.
/// * It fetches the **pair**. `segment_bytes` returns the `.dat` alone,
///   which is all a backup pass needs (it seals opaque bytes); adoption needs
///   the `.meta` too, because the sidecar is where `record_order` — and the
///   actor the segment belongs to — lives.
/// * It is keyed on the plane's `scope`, not on `(kind, scope_hex)`: the
///   account plane subscribes per scope, and a scope's segments span kinds.
///
/// An implementor over a real nest will adapt the shipped custodian-pull
/// mechanics rather than reimplement them — that is the reuse the charter
/// asks for, at the layer that can hold a network dependency.
#[allow(async_fn_in_trait)] // same posture as StoreBackend: static dispatch only
pub trait BootstrapSource {
    /// The segments this source holds for `scope`.
    async fn list_segments(&self, scope: &str) -> Result<Vec<SegmentOffer>>;

    /// Fetch one offered segment's `.dat` and `.meta` **into `into`**, chunk
    /// by chunk as the bytes arrive — never whole in memory: a segment has no
    /// size ceiling (`message-segment-store.md` § Segment size), so a
    /// transfer's memory is one chunk, and the store's staging slot is where
    /// the halves rest. Resume is the implementation's business;
    /// [`AccountStore::adopt_segment`]'s admission verifies what was staged.
    ///
    /// **Neither half may exceed `max_bytes`**, and an implementation over a
    /// network refuses a longer body *while reading it* (`u64::MAX` = the
    /// caller sets no budget) — the bound is what the transfer may write to
    /// disk. An implementation that knows the `.dat`'s declared size
    /// (`offer.dat_size`) also refuses a body longer than that.
    async fn fetch_segment(
        &self,
        scope: &str,
        offer: &SegmentOffer,
        max_bytes: u64,
        into: &mut impl SegmentSink,
    ) -> Result<()>;
}

/// What one scope's bulk bootstrap achieved.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BootstrapReport {
    pub adopted: usize,
    pub already_held: usize,
    /// Offers passed over because the policy dehydrates their kind — these
    /// scopes' indexes come from the feed walk instead, which is a *choice*
    /// and not a shortfall.
    pub skipped_dehydrating: usize,
    /// Offers refused because a kind did not match the scope they were
    /// offered under — either the offer's own `kind` (before the fetch) or
    /// the fetched sidecar's `kind` (at adoption). Counted rather than
    /// aborting the pull: one mislabelled segment must not cost the rest of
    /// the scope's bootstrap.
    pub refused_kind_mismatch: usize,
    /// Offers passed over because they would not fit the byte budget
    /// ([`AccountStore::bootstrap_scope_segments_within`]) — declared too
    /// large before the fetch, or found too large after it. Always 0 without
    /// a budget.
    pub skipped_over_budget: usize,
    pub records_indexed: usize,
}

/// A content scope's `(kind, scope-id)` pair — `kind` the head and `scope-id`
/// the hex-decoded tail of `content:<kind>:<scope-id-hex>` — or `None` for a
/// scope that is not a content scope. Spelled locally because this crate
/// deliberately holds no protocol dep (the encoding itself is pinned by
/// `fauna_protocol::scope`'s tests, and this file's own scope fixtures spell
/// it the same way). Adoption checks the pair against the scope it is filed
/// under — the id, which for a co-authored kind (`conv`) is a channel, never
/// this store's actor; and the kind, so a source cannot relabel a bulky
/// dehydrated kind as one the policy always hydrates — see
/// [`AccountStore::adopt_segment`].
fn content_scope_key(scope: &str) -> Option<(&str, [u8; 32])> {
    let rest = scope.strip_prefix("content:")?;
    let (kind, id_hex) = rest.split_once(':')?;
    let id = fauna_core::hex32::decode(id_hex).ok()?;
    Some((kind, id))
}

/// Verify the stored format pair against the binary's `(binary_v, binary_min)`
/// and stamp the store. Extracted from [`AccountStore::open`] (which calls it
/// with the real constants) so the stamping law is testable against injected
/// versions — the real floor sits at `1`, where every floor assertion on the
/// constants alone is vacuous.
pub(crate) async fn verify_and_stamp_format<B: StoreBackend>(
    backend: &B,
    binary_v: u16,
    binary_min: u16,
) -> Result<()> {
    debug_assert!(binary_min <= binary_v);
    // ONE read, not two gets: a sibling's cold open stamps the pair in one
    // transaction outside the migration lock, so two separate reads can take
    // `format_version` before that stamp and `min_reader_format_version`
    // after it — a half pair no store ever held, which the `(v, min)` arm
    // below then refuses as corrupt (measured under contention: two cold
    // assemblies on one store dir, the first failing its open).
    let [stored_v, stored_min]: [Option<Vec<u8>>; 2] = backend
        .meta_get_all(&[META_FORMAT_VERSION, META_MIN_READER])
        .await?
        .try_into()
        .map_err(|v: Vec<_>| anyhow::anyhow!("meta_get_all returned {} values, want 2", v.len()))?;
    let stored_v = parse_meta_u16(META_FORMAT_VERSION, stored_v)?;
    let stored_min = parse_meta_u16(META_MIN_READER, stored_min)?;
    // ONE transaction, not two puts (W5.3): the `(v, min)` arm below refuses
    // to guess at half a pair, so a second process cold-opening between two
    // separate puts would read a legitimately-written store as corrupt and
    // bail. And RISING-ONLY, because neither the migration lock (released
    // before adoption runs; degraded opens proceed unserialized) nor this
    // read-then-write can exclude a racing stamp — a version-skewed sibling's
    // write must max-merge, never clobber.
    let stamp = || {
        backend.meta_put_pair_max(
            (META_FORMAT_VERSION, binary_v),
            (META_MIN_READER, binary_min),
        )
    };
    match (stored_v, stored_min) {
        (None, None) => {
            stamp().await?;
        }
        (Some(v), Some(min)) => match check_format_compatibility(v, min, binary_v) {
            FormatVerdict::NewerBreaking => {
                return Err(StoreIncompatible {
                    format_version: v,
                    min_reader: min,
                    binary: binary_v,
                }
                .into());
            }
            // Tolerate a newer-additive store; never restamp down — and don't
            // raise the floor either: the newer binary's stamp describes the
            // store better than this one's, and floors are non-decreasing
            // along a real format lineage (the nest posture — `min_reader`
            // rides only when the version stamp does).
            FormatVerdict::NewerAdditive => {}
            FormatVerdict::Compatible => {
                // Restamp BOTH numbers up on every open — § 2.2's law (the
                // pair is re-recorded after every successful migration run;
                // the `max` idiom). A floor stamped
                // only at creation fails open on exactly the population it
                // exists for: the first breaking bump would refuse a store
                // this binary created and admit every store already in the
                // field. The guard on the write
                // makes the raise rise-only under races; the `if` only
                // avoids a redundant write transaction on the common path.
                if v < binary_v || min < binary_min {
                    stamp().await?;
                }
            }
        },
        (v, min) => bail!(
            "account store meta holds half a version pair (format_version {v:?}, \
             min_reader_format_version {min:?}) — refusing to guess"
        ),
    }
    Ok(())
}

fn parse_meta_u16(key: &str, bytes: Option<Vec<u8>>) -> Result<Option<u16>> {
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    let s = std::str::from_utf8(&bytes).with_context(|| format!("store meta {key}: not utf-8"))?;
    Ok(Some(s.parse().with_context(|| {
        format!("store meta {key}: not a u16: {s:?}")
    })?))
}

async fn adopt_identity<B: StoreBackend>(
    backend: &B,
    key: &str,
    ours: &[u8],
    what: &str,
) -> Result<()> {
    match backend.meta_get(key).await? {
        None => backend.meta_put(key, ours).await,
        Some(stored) if stored == ours => Ok(()),
        Some(_) => bail!(
            "account store belongs to a different {what} — refusing to open \
             (one store, one replica identity; charter § The store device principal)"
        ),
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::sqlite::SqliteBackend;

    use crate::conformance::fixtures::*;

    // The backend-generic tests live in `crate::conformance`, graded over
    // every arm; what stays here is what only the SQLite arm can show —
    // files on disk, a raw connection's view, the in-memory refusal.

    #[tokio::test]
    async fn a_newer_breaking_store_refuses_to_open_and_writes_nothing() {
        let b = SqliteBackend::open_in_memory().unwrap();
        b.meta_put(META_FORMAT_VERSION, b"9").await.unwrap();
        b.meta_put(META_MIN_READER, b"9").await.unwrap();
        let err = AccountStore::open(b, "aa11", writer(7)).await.unwrap_err();
        let incompatible = err.downcast_ref::<StoreIncompatible>().unwrap();
        assert_eq!(incompatible.min_reader, 9);
        // Nothing was written: inspect the file-backed case with a raw
        // connection — `SqliteBackend::open` itself now refuses a
        // newer-breaking store (the pre-migrate half of the same law), so
        // the backend is no vantage point onto one.
        let dir = tempfile::tempdir().unwrap();
        let b = SqliteBackend::open(dir.path()).unwrap();
        b.meta_put(META_FORMAT_VERSION, b"9").await.unwrap();
        b.meta_put(META_MIN_READER, b"9").await.unwrap();
        assert!(AccountStore::open(b, "aa11", writer(7)).await.is_err());
        let conn =
            rusqlite::Connection::open(dir.path().join(crate::sqlite::ACCOUNT_STORE_DB_FILENAME))
                .unwrap();
        let meta = |key: &str| -> Option<Vec<u8>> {
            use rusqlite::OptionalExtension;
            conn.query_row(
                "SELECT value FROM store_meta WHERE key = ?1",
                rusqlite::params![key],
                |r| r.get(0),
            )
            .optional()
            .unwrap()
        };
        assert_eq!(meta(META_ACTOR_ID), None);
        assert_eq!(meta(META_WRITER_ID), None);
        // And the stored pair was not restamped down.
        assert_eq!(meta(META_FORMAT_VERSION).unwrap(), b"9");
    }

    #[test]
    fn the_engine_lock_filename_is_reserved_beside_the_db() {
        assert_eq!(ENGINE_LOCK_FILENAME, "engine.lock");
        assert_eq!(
            engine_lock_path(Path::new("/x/actor")),
            Path::new("/x/actor/engine.lock")
        );
    }

    /// The charter's two critical sections take two files. A single shared
    /// lock file would serialize every cold open behind the engine role,
    /// which is held for the role's whole lifetime.
    #[test]
    fn the_migration_lock_filename_is_reserved_and_distinct() {
        assert_eq!(MIGRATION_LOCK_FILENAME, "migration.lock");
        assert_eq!(
            migration_lock_path(Path::new("/x/actor")),
            Path::new("/x/actor/migration.lock")
        );
        assert_ne!(ENGINE_LOCK_FILENAME, MIGRATION_LOCK_FILENAME);
    }

    #[test]
    fn format_verdicts() {
        use FormatVerdict::*;
        assert_eq!(check_format_compatibility(1, 1, 1), Compatible);
        assert_eq!(check_format_compatibility(2, 1, 1), NewerAdditive);
        assert_eq!(check_format_compatibility(2, 2, 1), NewerBreaking);
        assert_eq!(check_format_compatibility(1, 1, 2), Compatible);
    }

    // ── Segment adoption + bootstrap (W2.2) ──────────────────────────────────
    //
    // Every fixture here is written by the REAL segment writer
    // (`fauna_segment_store::FramedSegment`, a test-only dependency). The
    // account store is a reader of a format another crate owns, so a
    // hand-rolled imitation would only prove this module self-consistent — the
    // question these tests must answer is whether it adopts what the nest
    // actually writes.

    async fn store_on(dir: &Path) -> AccountStore<SqliteBackend> {
        AccountStore::open(
            SqliteBackend::open(dir).unwrap(),
            &seg_actor_hex(),
            writer(7),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn the_writers_real_segment_is_adopted_and_its_blocks_read_back() {
        // The parity test: bytes from the real writer, in through the store's
        // adoption door, out through the ordinary block API — and the caller
        // never learns the placement.
        let dir = tempfile::tempdir().unwrap();
        let s = store_on(dir.path()).await;
        let bodies: &[&[u8]] = &[b"sealed post one", b"sealed post two", b"sealed post three"];
        let (dat, meta) = write_segment("post", 4, SEG_ACTOR, bodies);

        assert_eq!(
            s.adopt_segment(&seg_post_scope(), &dat, &meta)
                .await
                .unwrap(),
            Some(3)
        );

        for body in bodies {
            let c = ContentHash::of_dag_cbor(body);
            assert_eq!(
                s.block(&c).await.unwrap().as_deref(),
                Some(&body[..]),
                "an adopted block reads back through the ordinary API"
            );
            assert!(s.is_present(&c).await.unwrap(), "and counts as held");
            let row = s.record(&c).await.unwrap().expect("indexed");
            assert_eq!(row.scope, seg_post_scope());
            assert_eq!(row.kind, "post");
            assert_eq!(row.size, Some(body.len() as u64));
        }

        // The files are kept verbatim — an adopted segment is still the CARv2
        // file the nest wrote, byte for byte, readable by any CARv2 tool.
        let held = s.adopted_segments(&seg_post_scope()).await.unwrap();
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].kind, "post");
        assert_eq!(held[0].segment_id, 4);
        let on_disk: Vec<Vec<u8>> = std::fs::read_dir(dir.path().join("segments"))
            .unwrap()
            .map(|e| std::fs::read(e.unwrap().path()).unwrap())
            .collect();
        assert!(on_disk.contains(&dat), "the .dat is stored byte-identical");
        assert!(on_disk.contains(&meta), "and so is the .meta");
    }

    #[tokio::test]
    async fn an_in_memory_store_says_so_rather_than_hiding_a_file_area() {
        // Segments are a *file* area beside the DB (charter § Physical
        // realization). An in-memory backend has none, and quietly conjuring a
        // temp dir would put user records somewhere the caller never named.
        let s = AccountStore::open(
            SqliteBackend::open_in_memory().unwrap(),
            &seg_actor_hex(),
            writer(7),
        )
        .await
        .unwrap();
        let (dat, meta) = write_segment("post", 1, SEG_ACTOR, &[b"x"]);
        let err = s
            .adopt_segment(&seg_post_scope(), &dat, &meta)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no segment area"), "got: {err}");
    }

    // ── Scope departure (T2 transition 3) ────────────────────────────────────

    /// A departed scope's adopted segments leave as **files**, which is the
    /// one reclamation `apply_tombstone` cannot do per record. Asserted on the
    /// file area, not just the routing rows: a row-only drop would leave the
    /// departed bytes readable on disk.
    #[tokio::test]
    async fn a_departure_removes_the_scopes_segment_files() {
        let dir = tempfile::tempdir().unwrap();
        let s = store_on(dir.path()).await;
        let scope = format!("content:post:{}", seg_actor_hex());
        let (dat, meta) = write_segment("post", 7, SEG_ACTOR, &[b"departed body"]);
        assert!(
            s.adopt_segment(&scope, &dat, &meta)
                .await
                .unwrap()
                .is_some()
        );
        let seg_dir = dir.path().join(crate::sqlite::SEGMENT_DIR_NAME);
        assert!(
            std::fs::read_dir(&seg_dir).unwrap().count() >= 2,
            "the pair is on disk before the departure"
        );

        let counts = s.drop_scope(&scope).await.unwrap();
        assert_eq!(counts.segments, 1, "{counts:?}");
        assert_eq!(
            std::fs::read_dir(&seg_dir).unwrap().count(),
            0,
            "the departed scope's segment files are gone from the file area"
        );
        assert!(s.adopted_segments(&scope).await.unwrap().is_empty());
        assert!(
            s.block(&ContentHash::of_dag_cbor(b"departed body"))
                .await
                .unwrap()
                .is_none(),
            "and its bytes are unreadable through the block API"
        );
    }

    /// The crash window: rows committed, files not yet swept. Reopening the
    /// store finishes the job — "atomic or resumable", never a half-departed
    /// scope (`nest/common.md` § Client-state recoverability).
    #[tokio::test]
    async fn a_departure_interrupted_before_its_file_sweep_resumes_at_open() {
        let dir = tempfile::tempdir().unwrap();
        let s = store_on(dir.path()).await;
        let scope = format!("content:post:{}", seg_actor_hex());
        let (dat, meta) = write_segment("post", 3, SEG_ACTOR, &[b"interrupted"]);
        s.adopt_segment(&scope, &dat, &meta).await.unwrap();

        // Exactly the state a crash between the commit and the sweep leaves:
        // the row transaction (which writes the pending mark) and nothing
        // after it.
        s.backend().drop_scope_rows_for_test(&scope).unwrap();
        let seg_dir = dir.path().join(crate::sqlite::SEGMENT_DIR_NAME);
        assert!(
            std::fs::read_dir(&seg_dir).unwrap().count() >= 2,
            "the files outlive the interrupted drop"
        );
        drop(s);

        let _reopened = store_on(dir.path()).await;
        assert_eq!(
            std::fs::read_dir(&seg_dir).unwrap().count(),
            0,
            "opening the store swept the files the interrupted departure owed"
        );
    }
}
