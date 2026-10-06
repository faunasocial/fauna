//! The native physical backend: one SQLite DB (WAL) per replica under
//! `<state base>/<actor-id-hex>/` (charter: `account-data-plane.md` § Store
//! logical schema, Physical realization).
//!
//! Every trait method is one transaction, per the [`StoreBackend`] contract;
//! multi-statement methods use an unchecked transaction (the connection is
//! never shared across threads — same shape as [`crate::db::SyncDb`]), and
//! every one that writes opens it through `SqliteBackend::write_tx`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use fauna_core::data::ContentHash;

use crate::backend::{
    ReadSeek, RelayEvicted, RelayScopeMeter, RetiredCompaction, ScopeDropCounts, SegmentScopeMeter,
    SegmentStaging, StoreBackend,
};
use crate::locks::{MigrationLock, MigrationLockOutcome};
use crate::segments::{AdoptedBlock, SEGMENT_TRANSFER_CHUNK, SegmentHalf, SegmentKey, SegmentSink};
use crate::types::{
    InsertOutcome, IntentDrainer, IntentStatus, IssuedRetire, ItemRef, JournalOp, JournalRow,
    NewOutboxIntent, OutboxIntent, RecordIndexEntry, RelayRow, StateEntry, WriterId,
};

/// The store DB's filename inside the per-actor store dir, beside
/// [`crate::store::ENGINE_LOCK_FILENAME`] and the segment/blob areas.
pub const ACCOUNT_STORE_DB_FILENAME: &str = "account-store.db";

/// The segment area: adopted CARv2 pairs live in `<store dir>/segments/`, as
/// files rather than DB blobs (charter § Store logical schema, Physical
/// realization: "the segment/blob **file areas** beside it" — web's parallel
/// backend puts the same bytes in OPFS). Keeping them as files is what lets
/// `go-car` / `kubo dag import` read an adopted segment unchanged.
pub const SEGMENT_DIR_NAME: &str = "segments";

use crate::physical::{
    META_SCOPE_DROPS_PENDING, STAGING_PREFIX, ascii_u64, entry_moved, nest_watermark_key,
    parse_pending_drops, scope_file_prefix, segment_stem, staging_stem,
};

/// A test's vantage point **inside** a multi-key `store_meta` transaction —
/// [`SqliteBackend::meta_put_pair_max`], between its two writes, and
/// [`StoreBackend::meta_get_all`], between its reads.
///
/// The property W5.3 (account-data-plane.md § Workstreams) needs from the pair write — *no concurrent reader ever
/// observes half a pair* — is a statement about a window that has closed by
/// the time the call returns. Asserting on the result instead would pass
/// identically against two separate `meta_put` calls, which is precisely the
/// regression that reopens the window. So the seam exists to let a test look
/// where the property lives, rather than to let it settle for a weaker one.
///
/// The multi-key READ needs the mirror of that, for the mirror reason: its
/// property is *no reader ever assembles a picture from two instants*, and a
/// caller-side assertion would pass identically against a loop of separate
/// `meta_get` calls — the very shape that let a fence land between the
/// re-author pass's stamp read and its marker read (charter § The store device
/// principal → succession decision 3).
/// The mechanism itself is `fauna_core::process_hook::ProcessHook` (round 116
/// lift — this module and `principal_succession::reauthor_window` hand-copied
/// it byte-for-byte before then).
#[cfg(test)]
mod pair_window {
    use fauna_core::process_hook::{Hook, Installed, ProcessHook};

    static HOOK: ProcessHook = ProcessHook::new();

    pub(super) fn install(hook: Hook) -> Installed {
        HOOK.install(hook)
    }

    pub(super) fn fire() {
        HOOK.fire();
    }
}

/// The account store's **genesis** — every table at its current shape, with
/// no step written for a store predating it (the compat-remnant sweep's
/// baseline reset, `version-compatibility.md` § Dimension 2). A nullable or
/// constant-default column added here reaches a long-lived store through
/// [`SqliteBackend::migrate`]'s `reconcile_added_columns`; anything outside
/// that additive class is refused at open.
const CREATE_TABLES_SQL: &str = "
    CREATE TABLE IF NOT EXISTS store_meta (
        key         TEXT PRIMARY KEY,
        value       BLOB NOT NULL
    );

    -- Per-writer append logs (charter § Store logical schema component 2).
    -- Rows are immutable once written; the PK is the log coordinate, and a
    -- log is per (scope, writer) — NOT per writer alone — because
    -- `WriterId::NEST_SEQUENCER` is a reserved *name* standing for whichever
    -- nest sequences a scope, and the nest's counter is per (scope, kind)
    -- (`records_db::next_changed_seq`): every content scope legitimately
    -- starts at seq 1 under the same name. A (writer_id, writer_seq) key
    -- refused the second walked scope's first row as equivocation.
    -- The equivocation check is accordingly per (scope, writer, seq); a
    -- device writer writes one scope today, so its log stays totally
    -- ordered — if class-2 ever publishes to several scopes (A5), the
    -- append counter's cross-scope story is that design's to rule.
    CREATE TABLE IF NOT EXISTS journal (
        writer_id   BLOB NOT NULL,
        writer_seq  INTEGER NOT NULL,
        scope       TEXT NOT NULL,
        op          TEXT NOT NULL,
        item_ref    BLOB NOT NULL,
        PRIMARY KEY (scope, writer_id, writer_seq)
    );
    -- The per-writer append counter's read path (`max_writer_seq`) — the PK
    -- no longer serves writer-first lookups.
    CREATE INDEX IF NOT EXISTS journal_writer
        ON journal (writer_id, writer_seq);
    -- The reverse lookup `coordinate_of_item` needs: item → the scope-feed
    -- coordinate the T1 seen-set intake itemizes. Index-only, no column
    -- change, so an older binary reads the same rows unchanged.
    CREATE INDEX IF NOT EXISTS journal_scope_item
        ON journal (scope, item_ref, writer_seq);

    -- Merged current value per (kind, key) (component 3). Value bytes are
    -- canonical dag-cbor and may be sealed — opaque here (R7 (account-data-plane.md § The ratified decisions)).
    CREATE TABLE IF NOT EXISTS state_entries (
        kind          TEXT NOT NULL,
        key           TEXT NOT NULL,
        scope         TEXT NOT NULL,
        value         BLOB NOT NULL,
        merge_meta    BLOB,
        entry_version INTEGER NOT NULL,
        tombstone     INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (kind, key)
    );

    -- The GROUP plane's merged current values — the group siblings of
    -- `state_entries`, one table per plane because the two registries are
    -- disjoint by law (`fauna_protocol::group_state`) and because a member
    -- belongs to MANY scopes: every scope has a `fauna.group.birth` row at
    -- the same logical key, so the account table's (kind, key) PK cannot
    -- hold them. Scope strings are `group:<scope-id-hex>`
    -- (`fauna_protocol::scope::GroupScope`). Journal rows, frontiers, and
    -- relay rows for group scopes ride the existing scope-keyed tables.
    -- Additive at-rest: a build predating this table simply holds no group
    -- entries.
    CREATE TABLE IF NOT EXISTS group_entries (
        scope         TEXT NOT NULL,
        kind          TEXT NOT NULL,
        key           TEXT NOT NULL,
        value         BLOB NOT NULL,
        merge_meta    BLOB,
        entry_version INTEGER NOT NULL,
        tombstone     INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (scope, kind, key)
    );

    -- Per-scope frontier vectors (component 4): one row per (scope, writer).
    CREATE TABLE IF NOT EXISTS frontiers (
        scope       TEXT NOT NULL,
        writer_id   BLOB NOT NULL,
        high_seq    INTEGER NOT NULL,
        PRIMARY KEY (scope, writer_id)
    );

    -- The record index (component 1): the ALWAYS-PRESENT layer. One row per
    -- class-1 record this replica knows of, held bytes or not. Presence is not
    -- a column — it is `EXISTS` in `blocks`, so the two cannot disagree.
    CREATE TABLE IF NOT EXISTS record_index (
        cid         BLOB PRIMARY KEY,
        scope       TEXT NOT NULL,
        kind        TEXT NOT NULL,
        size        INTEGER
    );
    CREATE INDEX IF NOT EXISTS record_index_scope
        ON record_index (scope, cid);

    -- Loose blocks (component 1): content-addressed bytes keyed by the fixed
    -- 36-byte CID. Bytes are opaque — the store never inspects them (R7).
    -- Adopted CARv2 segment files are the OTHER placement and land at W2.2;
    -- placement is invisible through the store API by design, so a block moving
    -- between the two is local compaction, not a plane event.
    CREATE TABLE IF NOT EXISTS blocks (
        cid         BLOB PRIMARY KEY,
        bytes       BLOB NOT NULL
    );

    -- Adopted segments (W2.2): the metadata half of the segment area. The
    -- bytes live as files under `<store dir>/segments/`; this table records
    -- which pairs are held and keeps each `.meta` sidecar verbatim, because an
    -- index rebuild reads `record_order` out of it. `dat_len` is the `.dat`
    -- length adoption wrote -- the custody meter's segment half reads it
    -- rather than the files -- and NULL once custody eviction has dropped the
    -- `.dat` (the row and its `.meta` stay: the metadata floor).
    CREATE TABLE IF NOT EXISTS segments (
        scope       TEXT NOT NULL,
        kind        TEXT NOT NULL,
        segment_id  INTEGER NOT NULL,
        meta        BLOB NOT NULL,
        dat_len     INTEGER,
        PRIMARY KEY (scope, kind, segment_id)
    );

    -- The routing table: which adopted segment holds a CID. This is a mirror
    -- of facts the segment files already carry (the nest keeps the same shape
    -- for the same reason -- `message-segment-store.md` § `segment_records`
    -- SQLite mirror), so it is rebuildable and never the authority. Offsets
    -- are deliberately absent: the read path uses the segment's own CARv2
    -- index, so no second offset table can drift from the file.
    CREATE TABLE IF NOT EXISTS segment_blocks (
        cid         BLOB NOT NULL,
        scope       TEXT NOT NULL,
        kind        TEXT NOT NULL,
        segment_id  INTEGER NOT NULL,
        len         INTEGER NOT NULL,
        PRIMARY KEY (cid, scope, kind, segment_id)
    );
    CREATE INDEX IF NOT EXISTS segment_blocks_seg
        ON segment_blocks (scope, kind, segment_id);

    -- The relay plane (W2.6, charter § The peer leg): one LIVE wire row per
    -- (scope, writer, item) — the nest's own per-(item, writer) collapse,
    -- mirrored so this replica can serve peers verbatim. `entry` is the sealed
    -- T14 envelope, opaque here (R7). Additive table: an older build ignores
    -- it, so the store's format_version pair is untouched.
    CREATE TABLE IF NOT EXISTS relay_rows (
        scope       TEXT NOT NULL,
        item_class  TEXT NOT NULL,
        writer_id   BLOB NOT NULL,
        writer_seq  INTEGER NOT NULL,
        item_key    BLOB NOT NULL,
        op          TEXT NOT NULL,
        entry       BLOB,
        -- The row's coordinate in the sequencing nest's log (`SyncChange.seq`;
        -- `RelayRow::feed_seq`, 2026-09-22): what the reclamation pass
        -- compares against the feed's `retirable_through_seq`. Nullable —
        -- unknown for a store-served row and an unpublished own row.
        feed_seq    INTEGER,
        -- The generation a form-v2 envelope's CLEARTEXT header names
        -- (`account_entry_crypto::peek_generation_id` — the same header a
        -- key-less custodian reads; nothing is opened), NULL for any other
        -- form. What the reclamation pass forgets a shredded generation's
        -- residue by (`account-data-taxonomy.md` § The generation machinery
        -- → *Fleet-scope reclamation*, clause (3)(h)); kept when payload
        -- eviction clears `entry`. Indexed after the column reconcile
        -- ([`RELAY_GENERATION_INDEX_SQL`]).
        generation_id BLOB,
        PRIMARY KEY (scope, writer_id, item_key)
    );
    CREATE INDEX IF NOT EXISTS relay_rows_walk
        ON relay_rows (scope, item_class, writer_id, writer_seq);
    -- The reclamation pass's by-item lookup (fleet-scope reclamation,
    -- 2026-09-16): which writers hold a live row for one blinded item key.
    -- Additive; an older build simply never uses it.
    CREATE INDEX IF NOT EXISTS relay_rows_item
        ON relay_rows (scope, item_class, item_key);

    -- The offline outbox (W4, charter § The offline-mutation contract — the
    -- phase-0 ruling): its own durable component, OfflineQueued intents only,
    -- transfer_queue discipline (per-intent rows, backoff column pair,
    -- completion-is-deletion). An undrained intent is the ONLY copy of a
    -- pending write, so this table is deliberately outside every scope-keyed
    -- plane: `drop_scope` must never name it. Additive at-rest: an older
    -- build ignores it, so the format_version pair is untouched.
    CREATE TABLE IF NOT EXISTS outbox (
        intent_id       BLOB PRIMARY KEY,
        kind            TEXT NOT NULL,
        scope           TEXT NOT NULL,
        payload         BLOB NOT NULL,
        drainer         TEXT NOT NULL,
        channel_seq     INTEGER NOT NULL,
        status          TEXT NOT NULL DEFAULT 'pending',
        retry_count     INTEGER NOT NULL DEFAULT 0,
        created_at      INTEGER NOT NULL,
        last_attempt_at INTEGER
    );
    CREATE UNIQUE INDEX IF NOT EXISTS outbox_scope_fifo
        ON outbox (scope, channel_seq);

    -- The retire record (`account-sync-plane.md` § The bind leg, ruling 5):
    -- every retire a bound plane sent, by its coordinates, belt and answer,
    -- for the secondary leg to re-issue at each linked nest. `ord` is the
    -- order token — rising with every put, so trimming to the cap keeps the
    -- newest. Recreatable: it is derived from retires already sent, and a
    -- lost entry delays its retire at a secondary or leaves a redundant row
    -- there (ruling 6).
    -- Additive at-rest: an older build ignores it, so the format_version pair
    -- is untouched.
    CREATE TABLE IF NOT EXISTS issued_retires (
        scope          TEXT NOT NULL,
        writer_id      BLOB NOT NULL,
        item_key       BLOB NOT NULL,
        writer_seq     INTEGER NOT NULL,
        ord            INTEGER NOT NULL,
        belt           BLOB,
        escrow_sweep   INTEGER NOT NULL DEFAULT 0,
        settled        INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (scope, writer_id, item_key, writer_seq)
    );
    CREATE UNIQUE INDEX IF NOT EXISTS issued_retires_ord
        ON issued_retires (ord);
";

/// The index over `relay_rows.generation_id` — run AFTER the column
/// reconcile, because on a long-lived store the genesis block's
/// `CREATE TABLE IF NOT EXISTS` leaves the table without the column until the
/// reconcile adds it. Partial: only form-v2 rows name a generation.
const RELAY_GENERATION_INDEX_SQL: &str = "
    CREATE INDEX IF NOT EXISTS relay_rows_generation
        ON relay_rows (scope, generation_id) WHERE generation_id IS NOT NULL;
";

pub struct SqliteBackend {
    conn: Connection,
    /// Where adopted segment files live. `None` for the in-memory backend —
    /// an in-memory store with a hidden on-disk file area would be a lie, so
    /// adoption refuses there and tests that exercise it open a real dir.
    segment_dir: Option<PathBuf>,
}

impl SqliteBackend {
    /// Open (or create) the store DB inside `store_dir`, in WAL mode
    /// (charter § Multi-instance concurrency: the store is
    /// multi-process-safe — WAL + busy-wait, transactions as the write unit).
    ///
    /// **The whole body is the migration/adoption critical section (W5.3).**
    /// WAL admits many concurrent readers and one writer, which is the
    /// posture for *steady-state* work but says nothing about two processes
    /// cold-opening one store dir at the same moment. Two things break there,
    /// and the charter's second store-level advisory lock ([`MigrationLock`]
    /// — § Multi-instance concurrency) is what closes both. It is taken
    /// **blocking**, before the connection, and released before this function
    /// returns: a live store handle never holds it, so a long-lived reader
    /// can never make a cold opener wait.
    ///
    /// 1. **The reconcile would run twice.** [`Self::migrate`]'s additive
    ///    reconcile probes each table's columns and then `ALTER`s in what is
    ///    missing, and a probe-then-rewrite pair is not a transaction — both
    ///    openers can read "column absent" and both run the `ALTER`, and the
    ///    second fails on the duplicate column.
    /// 2. **The WAL conversion fails outright** — measured 2026-08-14 by
    ///    `tests::concurrent_cold_opens_reconcile_the_store_once`,
    ///    which reported `set WAL: database is locked` on the third of four
    ///    racing openers *with `busy_timeout` already set*. Converting a
    ///    rollback-journal store to WAL needs a brief exclusive lock, and
    ///    `PRAGMA journal_mode` does not consult the busy handler: it returns
    ///    SQLITE_BUSY immediately rather than waiting. So the timeout is not
    ///    a substitute for the lock here — it is why the *pragma* is inside
    ///    the section and not merely `migrate()`.
    ///
    /// A [`Degraded`](MigrationLockOutcome::Degraded) lock proceeds anyway.
    /// That is the shipped `AccountInstanceLock` posture (degrade OPEN, never
    /// closed), and it is the *right* posture here specifically: a store dir
    /// whose lock file cannot be opened is one where nothing else can be
    /// running either, so what a degrade admits is the single-process open —
    /// every open before W5 — and refusing would turn a lock-file problem
    /// into an unopenable store.
    pub fn open(store_dir: impl AsRef<Path>) -> Result<Self> {
        let store_dir = store_dir.as_ref();
        std::fs::create_dir_all(store_dir)
            .with_context(|| format!("create store dir {}", store_dir.display()))?;
        Self::open_in_dir(store_dir, OpenFlags::default())
    }

    /// [`Self::open`] for a store that may not exist: `None` when `store_dir`
    /// holds no store database, and nothing is created — no directory, no
    /// lock file, no database. The reader that must never mint a store is the
    /// pre-login local read (`nest/box-recovery.md` § The plane-era recovery
    /// floor, *(b)*), which runs for accounts this device may never have held.
    ///
    /// The database opens **without** SQLite's create flag, so a store erased
    /// between the existence check and the open (a sign-out racing the read)
    /// fails the open rather than being re-created as an empty database (the
    /// migration section's lock file is the one thing that window can leave
    /// behind). An existing store
    /// opens exactly as [`Self::open`] opens it — the same migration section
    /// and WAL posture, so it is as safe beside a process hosting the engine
    /// as every other opener.
    pub fn open_existing(store_dir: impl AsRef<Path>) -> Result<Option<Self>> {
        let store_dir = store_dir.as_ref();
        if !store_dir.join(ACCOUNT_STORE_DB_FILENAME).is_file() {
            return Ok(None);
        }
        let flags = OpenFlags::default() - OpenFlags::SQLITE_OPEN_CREATE;
        Self::open_in_dir(store_dir, flags).map(Some)
    }

    /// The open sequence both doors share, over a directory that exists.
    fn open_in_dir(store_dir: &Path, flags: OpenFlags) -> Result<Self> {
        let _section = match MigrationLock::acquire(store_dir) {
            MigrationLockOutcome::Held(lock) => Some(lock),
            MigrationLockOutcome::Degraded(e) => {
                tracing::warn!(
                    error = %e,
                    store_dir = %store_dir.display(),
                    "account store: migration lock unavailable — opening unserialized \
                     (safe for the single-process case; see SqliteBackend::open)"
                );
                None
            }
        };
        let conn = Connection::open_with_flags(store_dir.join(ACCOUNT_STORE_DB_FILENAME), flags)
            .context("open account-store sqlite")?;
        // For the store's own statements. NOT what protects the pragma below
        // — see point 2 above.
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .context("set busy_timeout")?;
        // § 2.2's boot-check law: the compatibility verdict runs BEFORE
        // anything mutates (the nest's check-in-`CacheDb::open` placement,
        // `version-compatibility.md` § 2.2). The full verdict lives
        // backend-generically in `AccountStore::open`; this early read exists
        // because THIS backend physically migrates during its own open,
        // before the store ever sees the pair — and a newer-breaking store
        // must not be WAL-converted or migrated by a binary that will then
        // refuse it. Today's migrations happen to no-op on a future-format
        // store; the next one need not.
        Self::refuse_newer_breaking(&conn)?;
        // WAL is the multi-process posture; a query_row because journal_mode
        // replies with the resulting mode.
        let _mode: String = conn
            .query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
            .context("set WAL")?;
        let backend = Self {
            conn,
            segment_dir: Some(store_dir.join(SEGMENT_DIR_NAME)),
        };
        backend.migrate()?;
        Ok(backend)
    }

    /// The pre-migrate half of the format verdict (see the call site in
    /// [`Self::open`]): refuse a stored pair whose floor this binary cannot
    /// meet, with the same typed error the store-level check raises. Only the
    /// whole-pair breaking case refuses here — absent or half pairs are
    /// [`AccountStore`](crate::store::AccountStore)'s to rule on (it refuses
    /// to guess), and migrating under them is what every open already does.
    fn refuse_newer_breaking(conn: &Connection) -> Result<()> {
        let has_meta: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master \
                 WHERE type = 'table' AND name = 'store_meta')",
                [],
                |r| r.get(0),
            )
            .context("probe store_meta")?;
        if !has_meta {
            // Fresh store — nothing stamped yet.
            return Ok(());
        }
        let read_u16 = |key: &str| -> Result<Option<u16>> {
            let bytes: Option<Vec<u8>> = conn
                .query_row(
                    "SELECT value FROM store_meta WHERE key = ?1",
                    params![key],
                    |r| r.get(0),
                )
                .optional()
                .with_context(|| format!("store_meta get {key}"))?;
            bytes
                .map(|b| -> Result<u16> {
                    let s = std::str::from_utf8(&b)
                        .with_context(|| format!("store meta {key}: not utf-8"))?;
                    s.parse()
                        .with_context(|| format!("store meta {key}: not a u16: {s:?}"))
                })
                .transpose()
        };
        use crate::store::{
            FORMAT_VERSION, FormatVerdict, META_FORMAT_VERSION, META_MIN_READER, StoreIncompatible,
            check_format_compatibility,
        };
        if let (Some(v), Some(min)) = (read_u16(META_FORMAT_VERSION)?, read_u16(META_MIN_READER)?)
            && check_format_compatibility(v, min, FORMAT_VERSION) == FormatVerdict::NewerBreaking
        {
            return Err(StoreIncompatible {
                format_version: v,
                min_reader: min,
                binary: FORMAT_VERSION,
            }
            .into());
        }
        Ok(())
    }

    /// In-memory store (tests). No WAL — memory DBs don't support it, and
    /// nothing multi-process can reach one anyway. No segment area either:
    /// [`StoreBackend::segment_adopt`] refuses rather than inventing a
    /// temp dir behind the caller's back.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("open in-memory sqlite")?;
        let backend = Self {
            conn,
            segment_dir: None,
        };
        backend.migrate()?;
        Ok(backend)
    }

    /// The `.dat`/`.meta` paths for one adopted segment. The scope is
    /// percent-ish-escaped into a single filename component: scopes carry `:`
    /// (`content:__post`) and `/` would otherwise mint directories.
    fn segment_paths(&self, key: &SegmentKey) -> Result<(PathBuf, PathBuf)> {
        let Some(dir) = self.segment_dir.as_ref() else {
            bail!(
                "this store has no segment area (in-memory backend) — \
                 adopt segments against a store opened on a directory"
            );
        };
        let stem = segment_stem(key);
        Ok((
            dir.join(format!("{stem}.dat")),
            dir.join(format!("{stem}.meta")),
        ))
    }

    fn migrate(&self) -> Result<()> {
        self.conn
            .execute_batch(CREATE_TABLES_SQL)
            .context("account-store genesis")?;
        // The additive column reconciler every genesis in the tree pairs with
        // its block: a column the block declares that a long-lived store lacks
        // is added here, under the migration lock `open` holds.
        fauna_core::sqlite_schema_meta::reconcile_added_columns(&self.conn, |reference| {
            Ok(reference.execute_batch(CREATE_TABLES_SQL)?)
        })
        .context("account-store column reconcile")?;
        self.conn
            .execute_batch(RELAY_GENERATION_INDEX_SQL)
            .context("account-store relay generation index")?;
        // A scope departure whose row transaction committed but whose segment
        // files had not yet been swept when the process died (T2 transition 3
        // — see `drop_scope`). Replayed here so the departed bytes never
        // outlive one launch; a store with no pending mark pays one meta read.
        self.resume_pending_scope_drops()
            .context("account-store resume pending scope drops")?;
        // A segment transfer the process died in the middle of leaves its
        // staging files and nothing else (adoption renames before it writes a
        // row). Swept here, like the drop mark above.
        self.sweep_staged_segments()
            .context("account-store sweep staged segments")
            .map(|_swept| ())
    }

    /// Remove every staging file ([`SqliteStaging`]) no live process holds —
    /// the open-time sweep of a crashed transfer's leftovers. A file whose
    /// lock another process holds is a transfer in flight, and is left alone.
    fn sweep_staged_segments(&self) -> Result<u64> {
        let Some(dir) = self.segment_dir.as_ref() else {
            return Ok(0); // in-memory backend: no file area
        };
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => {
                return Err(e).with_context(|| format!("read segment area {}", dir.display()));
            }
        };
        let mut removed = 0;
        for entry in entries {
            let entry = entry.with_context(|| format!("scan segment area {}", dir.display()))?;
            if !entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(STAGING_PREFIX))
            {
                continue;
            }
            let path = entry.path();
            let Ok(file) = std::fs::File::open(&path) else {
                continue; // gone already — another opener's sweep
            };
            if file.try_lock().is_err() {
                continue; // a live transfer holds it
            }
            // Closed before the remove: Windows refuses to delete a file this
            // process holds open.
            drop(file);
            match std::fs::remove_file(&path) {
                Ok(()) => removed += 1,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(e)
                        .with_context(|| format!("remove staged segment {}", path.display()));
                }
            }
        }
        Ok(removed)
    }

    /// Finish any [`StoreBackend::drop_scope`] that got as far as committing
    /// its rows. The mark is the list of scopes whose files may still be on
    /// disk; sweeping is idempotent, so replaying a sweep that already ran is
    /// free.
    fn resume_pending_scope_drops(&self) -> Result<()> {
        for scope in self.pending_scope_drops()? {
            self.sweep_scope_segment_files(&scope)?;
            self.clear_pending_scope_drop(&scope)?;
        }
        Ok(())
    }

    fn pending_scope_drops(&self) -> Result<Vec<String>> {
        let raw: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT value FROM store_meta WHERE key = ?1",
                params![META_SCOPE_DROPS_PENDING],
                |r| r.get(0),
            )
            .optional()
            .context("read pending scope drops")?;
        let Some(raw) = raw else {
            return Ok(Vec::new());
        };
        Ok(parse_pending_drops(&raw))
    }

    fn clear_pending_scope_drop(&self, scope: &str) -> Result<()> {
        let rest: Vec<String> = self
            .pending_scope_drops()?
            .into_iter()
            .filter(|s| s != scope)
            .collect();
        if rest.is_empty() {
            self.conn
                .execute(
                    "DELETE FROM store_meta WHERE key = ?1",
                    params![META_SCOPE_DROPS_PENDING],
                )
                .context("clear pending scope drops")?;
        } else {
            self.conn
                .execute(
                    "INSERT INTO store_meta (key, value) VALUES (?1, ?2)
                     ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                    params![META_SCOPE_DROPS_PENDING, rest.join("\n").as_bytes()],
                )
                .context("rewrite pending scope drops")?;
        }
        Ok(())
    }

    /// Delete every adopted segment file belonging to `scope`.
    ///
    /// Keyed on the **filename prefix** rather than on surviving `segments`
    /// rows, because by the time this runs those rows are gone (that is what
    /// makes the sweep replayable after a crash). The prefix is unambiguous:
    /// [`segment_paths`](Self::segment_paths) builds every stem as
    /// `<scope>-<kind>-seg-<id>`, and a scope string ends in a fixed-width hex
    /// id, so no scope's sanitized form is a prefix of another's followed by
    /// `-`.
    fn sweep_scope_segment_files(&self, scope: &str) -> Result<u64> {
        let Some(dir) = self.segment_dir.as_ref() else {
            return Ok(0); // in-memory backend: no file area to sweep
        };
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            // No segment area yet — nothing was ever adopted.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => {
                return Err(e).with_context(|| format!("read segment area {}", dir.display()));
            }
        };
        let prefix = scope_file_prefix(scope);
        let mut removed = 0;
        for entry in entries {
            let entry = entry.with_context(|| format!("scan segment area {}", dir.display()))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if !name.starts_with(&prefix) {
                continue;
            }
            let path = entry.path();
            match std::fs::remove_file(&path) {
                Ok(()) => removed += 1,
                // Lost a race with another sweep of the same scope — the
                // outcome we wanted either way.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(e)
                        .with_context(|| format!("remove departed segment {}", path.display()));
                }
            }
        }
        Ok(removed)
    }

    /// Open a transaction that writes: `BEGIN IMMEDIATE`, so the write lock
    /// is taken — and waited for, under `busy_timeout` — before the first
    /// statement.
    ///
    /// Every multi-statement write in this backend opens through here. A
    /// deferred transaction that reads before it writes is a *reader* asking
    /// for the write lock, and SQLite never runs the busy handler for that
    /// request: while a sibling process holds the lock, or once one has
    /// committed past the reader's snapshot, the write answers `database is
    /// locked` at once. With several same-account processes on one store
    /// (charter § Multi-instance concurrency → *Store contract*) that is a
    /// local write refused because another instance happened to be mid-commit
    /// — pinned by `a_guarded_local_write_waits_behind_a_siblings_write_lock`.
    ///
    /// It is also what makes a read inside the transaction a fact for the
    /// whole of it: no other connection can commit between that read and this
    /// transaction's own commit.
    fn write_tx(&self) -> rusqlite::Result<rusqlite::Transaction<'_>> {
        rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)
    }

    /// The row half of a scope departure: one transaction, every plane.
    ///
    /// The pending mark is written **inside** it, so "rows gone" and "files
    /// owed" commit together — the property the resume leans on.
    fn drop_scope_rows(&self, scope: &str) -> Result<ScopeDropCounts> {
        let tx = self.write_tx().context("drop_scope tx")?;
        let delete = |sql: &str, what: &'static str| -> Result<u64> {
            Ok(self.conn.execute(sql, params![scope]).context(what)? as u64)
        };

        // Loose block bytes go with their index rows, so this must precede the
        // index delete that names them. Segment-resident bytes are NOT deleted
        // here — they leave with the segment files after the commit, the only
        // way to reclaim a byte from an immutable CARv2 (the
        // `apply_tombstone` / `dehydrate` rule, applied per scope).
        let blocks = delete(
            "DELETE FROM blocks WHERE cid IN
                 (SELECT cid FROM record_index WHERE scope = ?1)",
            "drop scope blocks",
        )?;
        let counts = ScopeDropCounts {
            blocks,
            record_index_rows: delete(
                "DELETE FROM record_index WHERE scope = ?1",
                "drop scope record index",
            )?,
            journal_rows: delete("DELETE FROM journal WHERE scope = ?1", "drop scope journal")?,
            // Entries *of* the departed scope. The account's seen-set entry FOR
            // that scope is untouched by construction: it lives in the
            // account-state scope and merely carries the scope string as its
            // key (charter T2 transition 4 — the set is grow-only, membership
            // never shrinks).
            state_entries: delete(
                "DELETE FROM state_entries WHERE scope = ?1",
                "drop scope state entries",
            )?,
            frontier_rows: delete(
                "DELETE FROM frontiers WHERE scope = ?1",
                "drop scope frontier",
            )?,
            relay_rows: delete(
                "DELETE FROM relay_rows WHERE scope = ?1",
                "drop scope relay rows",
            )?,
            segments: {
                // The routing mirror first: it is rebuildable from the files
                // and never the authority, so it carries no count of its own.
                delete(
                    "DELETE FROM segment_blocks WHERE scope = ?1",
                    "drop scope segment routing",
                )?;
                delete(
                    "DELETE FROM segments WHERE scope = ?1",
                    "drop scope segments",
                )?
            },
        };

        // The scope's serve-order watermark leaves with the rows it vouched
        // for: left behind, it would tell a re-joined scope's first walk that
        // this store holds a prefix of the nest's log it no longer has.
        self.conn
            .execute(
                "DELETE FROM store_meta WHERE key = ?1",
                params![nest_watermark_key(scope)],
            )
            .context("drop scope watermark")?;

        if counts.segments > 0 {
            let mut pending = self.pending_scope_drops()?;
            if !pending.iter().any(|s| s == scope) {
                pending.push(scope.to_owned());
            }
            self.conn
                .execute(
                    "INSERT INTO store_meta (key, value) VALUES (?1, ?2)
                     ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                    params![META_SCOPE_DROPS_PENDING, pending.join("\n").as_bytes()],
                )
                .context("mark scope drop pending")?;
        }

        tx.commit().context("commit scope drop")?;
        Ok(counts)
    }

    /// The row half of a departure **alone** — the exact state a crash between
    /// the commit and the file sweep leaves behind. Test-only: production
    /// always continues into the sweep (see `drop_scope`), and the only way to
    /// prove the resume works is to stop there deliberately.
    #[cfg(test)]
    pub fn drop_scope_rows_for_test(&self, scope: &str) -> Result<ScopeDropCounts> {
        self.drop_scope_rows(scope)
    }

    /// One `store_meta` read. Shared by the single reader and the multi-key
    /// snapshot/compare so they cannot drift — the same statement, inside
    /// whatever transaction the caller has open.
    fn meta_get_sync(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.conn
            .query_row(
                "SELECT value FROM store_meta WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()
            .context("store_meta get")
    }

    /// One `store_meta` upsert. Shared by the single and pair writers so the
    /// pair cannot drift from the single (the pair is the same statement,
    /// twice, inside one transaction).
    fn meta_put_sync(&self, key: &str, value: &[u8]) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO store_meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .context("store_meta put")?;
        Ok(())
    }

    /// One rising-only `store_meta` upsert: lands only when `value` exceeds
    /// what is stored ([`StoreBackend::meta_put_pair_max`]'s per-key guard).
    /// The `CAST`s are load-bearing — values rest as ASCII-decimal blobs, and
    /// a bytewise compare would order `"10"` below `"2"`.
    fn meta_put_u16_max_sync(&self, key: &str, value: u16) -> Result<()> {
        self.meta_put_u64_max_sync(key, u64::from(value))
    }

    /// [`Self::meta_put_u16_max_sync`] at the width a nest-log `seq` needs —
    /// the one statement both widths share. SQLite's `INTEGER` is a signed
    /// 64-bit, so the value is refused above `i64::MAX` rather than wrapped
    /// into a compare that would read it as negative.
    fn meta_put_u64_max_sync(&self, key: &str, value: u64) -> Result<()> {
        i64::try_from(value).context("rising-only meta value exceeds i64")?;
        self.conn
            .execute(
                "INSERT INTO store_meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value
                 WHERE CAST(store_meta.value AS INTEGER) < CAST(excluded.value AS INTEGER)",
                params![key, value.to_string().into_bytes()],
            )
            .context("store_meta rising-only put")?;
        Ok(())
    }

    /// The append-time writer guard ([`StoreBackend::insert_row`]'s
    /// `local_writer` contract): compare the store's writer-identity meta
    /// against the appending handle's writer. The caller must already be
    /// inside the transaction the guarded write runs in — that is what makes
    /// "checked inside the append transaction" true: the transaction holds
    /// the write lock from its `BEGIN` ([`Self::write_tx`]), so no re-stamp
    /// can commit between this read and the row landing.
    fn writer_guard_sync(&self, local_writer: Option<&WriterId>) -> Result<()> {
        let Some(held) = local_writer else {
            return Ok(());
        };
        let stored: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT value FROM store_meta WHERE key = ?1",
                params![crate::store::META_WRITER_ID],
                |r| r.get(0),
            )
            .optional()
            .context("writer guard: read writer identity")?;
        match stored {
            // A store is stamped at open, before any local append — an absent
            // identity is a fresh backend under test, not a rotation.
            None => Ok(()),
            Some(v) if v.as_slice() == held.0 => Ok(()),
            Some(v) => {
                let current: [u8; 32] = v
                    .as_slice()
                    .try_into()
                    .context("writer guard: stored writer identity is not 32 bytes")?;
                Err(crate::store::StaleWriter {
                    held: *held,
                    current: WriterId(current),
                }
                .into())
            }
        }
    }

    /// Insert one row; report what occupies the slot if the insert lost.
    /// Safe without an explicit transaction: journal rows are immutable, so
    /// the compare can never race an update — only another identical-logic
    /// inserter, which yields the same verdict.
    fn insert_row_sync(&self, row: &JournalRow) -> Result<InsertOutcome> {
        let inserted = self.conn.execute(
            "INSERT OR IGNORE INTO journal (writer_id, writer_seq, scope, op, item_ref)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                row.writer.0.as_slice(),
                i64::try_from(row.seq).context("writer_seq exceeds i64")?,
                row.scope,
                row.op.as_str(),
                row.item.encode(),
            ],
        )?;
        if inserted == 1 {
            return Ok(InsertOutcome::Inserted);
        }
        let existing = self
            .conn
            .query_row(
                "SELECT op, item_ref FROM journal
                 WHERE scope = ?1 AND writer_id = ?2 AND writer_seq = ?3",
                params![
                    row.scope,
                    row.writer.0.as_slice(),
                    i64::try_from(row.seq).context("writer_seq exceeds i64")?
                ],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)),
            )
            .context("read occupant of journal slot")?;
        let identical = existing.0 == row.op.as_str() && existing.1 == row.item.encode();
        Ok(if identical {
            InsertOutcome::IdenticalPresent
        } else {
            InsertOutcome::OccupiedByDifferent
        })
    }

    /// Idempotent content-addressed insert. `INSERT OR IGNORE` rather than an
    /// upsert: the key *is* the hash of the value, so an occupied slot already
    /// holds byte-identical content (the store layer verifies the CID before
    /// calling, so a mismatched pair never reaches here).
    fn block_put_sync(&self, cid: &ContentHash, bytes: &[u8]) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR IGNORE INTO blocks (cid, bytes) VALUES (?1, ?2)",
                params![cid.as_bytes().as_slice(), bytes],
            )
            .context("blocks put")?;
        Ok(())
    }

    fn record_index_put_sync(&self, entry: &RecordIndexEntry) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO record_index (cid, scope, kind, size) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (cid) DO UPDATE SET
                     scope = excluded.scope,
                     kind = excluded.kind,
                     size = COALESCE(excluded.size, size)",
                params![
                    entry.cid.as_bytes().as_slice(),
                    entry.scope,
                    entry.kind,
                    entry
                        .size
                        .map(i64::try_from)
                        .transpose()
                        .context("record size exceeds i64")?,
                ],
            )
            .context("record_index put")?;
        Ok(())
    }

    fn segment_of_block_sync(&self, cid: &ContentHash) -> Result<Option<SegmentKey>> {
        self.conn
            .query_row(
                "SELECT scope, kind, segment_id FROM segment_blocks WHERE cid = ?1 LIMIT 1",
                params![cid.as_bytes().as_slice()],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()
            .context("segment_blocks route")?
            .map(|(scope, kind, segment_id)| {
                Ok(SegmentKey {
                    scope,
                    kind,
                    segment_id: u32::try_from(segment_id).context("segment_id out of range")?,
                })
            })
            .transpose()
    }

    fn row_from_sql(
        writer: Vec<u8>,
        seq: i64,
        scope: String,
        op: String,
        item_ref: Vec<u8>,
    ) -> Result<JournalRow> {
        Ok(JournalRow {
            writer: WriterId(
                writer
                    .as_slice()
                    .try_into()
                    .context("journal writer_id is not 32 bytes")?,
            ),
            seq: u64::try_from(seq).context("negative writer_seq")?,
            scope,
            op: JournalOp::parse(&op)?,
            item: ItemRef::decode(&item_ref)?,
        })
    }
}

/// The one relay-plane delete by coordinate — every row at `(scope, writer,
/// writer_seq)`, optionally sparing one item. The compaction's and
/// `relay_retire_at`'s whole-coordinate form (`keep_item_key = None`) and
/// `relay_retire_shadowed`'s carve-out (`Some`) share it, so the coordinate
/// predicate is spelled once. Returns the rows deleted.
fn delete_relay_rows_at(
    conn: &Connection,
    scope: &str,
    writer: &WriterId,
    writer_seq: i64,
    keep_item_key: Option<&[u8]>,
) -> Result<usize> {
    let deleted = match keep_item_key {
        None => conn.execute(
            "DELETE FROM relay_rows
             WHERE scope = ?1 AND writer_id = ?2 AND writer_seq = ?3",
            params![scope, writer.0.as_slice(), writer_seq],
        ),
        Some(keep) => conn.execute(
            "DELETE FROM relay_rows
             WHERE scope = ?1 AND writer_id = ?2 AND writer_seq = ?3
               AND item_key != ?4",
            params![scope, writer.0.as_slice(), writer_seq, keep],
        ),
    }
    .context("relay_rows delete at coordinate")?;
    Ok(deleted)
}

/// A stored `relay_rows.feed_seq` as the row's `Option<u64>` — a negative
/// value is no coordinate in any log and is an error, never a silent `None`.
fn relay_feed_seq(stored: Option<i64>) -> Result<Option<u64>> {
    stored
        .map(|s| u64::try_from(s).context("negative relay feed_seq"))
        .transpose()
}

impl StoreBackend for SqliteBackend {
    type Staging = SqliteStaging;

    /// `PRAGMA data_version` — moves when another connection commits to this
    /// database file, never from this connection's own writes (the trait's
    /// contract, verbatim SQLite semantics). One connection per backend
    /// (`self.conn`), so "this connection" is exactly "this backend".
    async fn data_version(&self) -> Result<Option<u64>> {
        let v: i64 = self
            .conn
            .query_row("PRAGMA data_version", [], |r| r.get(0))
            .context("data_version")?;
        Ok(Some(v as u64))
    }

    async fn meta_get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.meta_get_sync(key)
    }

    /// One deferred transaction: in WAL mode its read snapshot is fixed by
    /// the first `SELECT` and held to the commit, so every key here is read
    /// as of one instant — a fence landing mid-loop is invisible to us and
    /// lands wholly before or wholly after. Read-only, so it is the one
    /// transaction here that does not open through [`Self::write_tx`]: a
    /// reader must never queue behind a writer.
    async fn meta_get_all(&self, keys: &[&str]) -> Result<Vec<Option<Vec<u8>>>> {
        let tx = self
            .conn
            .unchecked_transaction()
            .context("store_meta multi-get tx")?;
        let mut out = Vec::with_capacity(keys.len());
        for key in keys {
            out.push(self.meta_get_sync(key)?);
            #[cfg(test)]
            pair_window::fire();
        }
        tx.commit().context("store_meta multi-get commit")?;
        Ok(out)
    }

    async fn meta_put(&self, key: &str, value: &[u8]) -> Result<()> {
        self.meta_put_sync(key, value)
    }

    async fn meta_put_all(&self, pairs: &[(&str, &[u8])]) -> Result<()> {
        let tx = self.write_tx().context("store_meta multi-put tx")?;
        for (key, value) in pairs {
            self.meta_put_sync(key, value)?;
        }
        tx.commit().context("store_meta multi-put commit")
    }

    async fn meta_delete(&self, key: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM store_meta WHERE key = ?1", params![key])
            .context("store_meta delete")?;
        Ok(())
    }

    /// The compare and the deletes share one transaction, which holds the
    /// write lock from its `BEGIN` ([`Self::write_tx`]) — nothing can commit
    /// between the compare and the deletes, the same argument
    /// [`Self::writer_guard_sync`] rests on. The explicit compare is the
    /// contract: it is what makes the refusal a reportable verdict here and
    /// on a backend with no write lock to borrow (web's IndexedDB, W6).
    async fn meta_delete_all_if_unchanged(
        &self,
        expected: &[(&str, Option<&[u8]>)],
    ) -> Result<bool> {
        let tx = self
            .write_tx()
            .context("store_meta compare-and-delete tx")?;
        for (key, want) in expected {
            if self.meta_get_sync(key)?.as_deref() != *want {
                // Roll back by drop; nothing was written on this path.
                return Ok(false);
            }
        }
        for (key, _) in expected {
            self.conn
                .execute("DELETE FROM store_meta WHERE key = ?1", params![key])
                .context("store_meta compare-and-delete")?;
        }
        tx.commit()
            .context("store_meta compare-and-delete commit")?;
        Ok(true)
    }

    async fn scopes_of_writer(&self, writer: &WriterId) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT scope FROM journal WHERE writer_id = ?1 ORDER BY scope")
            .context("prepare scopes_of_writer")?;
        let rows = stmt
            .query_map(params![writer.0.as_slice()], |r| r.get::<_, String>(0))
            .context("query scopes_of_writer")?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .context("read scopes_of_writer")
    }

    async fn compact_retired_rows(&self, rows: &[JournalRow]) -> Result<RetiredCompaction> {
        let tx = self.write_tx().context("retired compaction tx")?;
        let stamped = self.meta_get_sync(crate::store::META_WRITER_ID)?;
        let mut done = RetiredCompaction::default();
        for row in rows {
            if stamped.as_deref() == Some(row.writer.0.as_slice()) {
                // Roll back by drop: nothing of this call lands.
                bail!(
                    "retired compaction: writer {} is the store's current writer — its log is \
                     never compacted (its append counter would re-issue the freed seqs)",
                    row.writer.to_hex()
                );
            }
            let seq = i64::try_from(row.seq).context("writer_seq exceeds i64")?;
            let deleted = self
                .conn
                .execute(
                    "DELETE FROM journal
                     WHERE scope = ?1 AND writer_id = ?2 AND writer_seq = ?3
                       AND op = ?4 AND item_ref = ?5",
                    params![
                        row.scope,
                        row.writer.0.as_slice(),
                        seq,
                        row.op.as_str(),
                        row.item.encode()
                    ],
                )
                .context("retired compaction: journal")?;
            if deleted == 0 {
                continue;
            }
            done.journal_rows += deleted;
            done.relay_rows += delete_relay_rows_at(&self.conn, &row.scope, &row.writer, seq, None)
                .context("retired compaction: relay plane")?;
        }
        tx.commit().context("retired compaction commit")?;
        Ok(done)
    }

    async fn meta_put_pair_max(&self, first: (&str, u16), second: (&str, u16)) -> Result<()> {
        let tx = self.write_tx().context("store_meta pair tx")?;
        self.meta_put_u16_max_sync(first.0, first.1)?;
        #[cfg(test)]
        pair_window::fire();
        self.meta_put_u16_max_sync(second.0, second.1)?;
        tx.commit().context("store_meta pair commit")
    }

    async fn insert_row(
        &self,
        row: &JournalRow,
        local_writer: Option<&WriterId>,
    ) -> Result<InsertOutcome> {
        match local_writer {
            // Unguarded (ingest): single-statement, no transaction needed —
            // journal rows are immutable, so the occupant compare can never
            // race an update.
            None => self.insert_row_sync(row),
            // Guarded (local append): the identity read and the insert must
            // share one transaction (the append-time writer guard contract).
            Some(_) => {
                let tx = self.write_tx().context("guarded insert_row tx")?;
                self.writer_guard_sync(local_writer)?;
                let outcome = self.insert_row_sync(row)?;
                tx.commit().context("guarded insert_row commit")?;
                Ok(outcome)
            }
        }
    }

    async fn state_put_with_row(
        &self,
        entry: &StateEntry,
        row: &JournalRow,
        local_writer: Option<&WriterId>,
    ) -> Result<InsertOutcome> {
        let tx = self.write_tx().context("state_put_with_row tx")?;
        self.writer_guard_sync(local_writer)?;
        let stored = self.state_get(&entry.kind, &entry.key).await?;
        if entry_moved(stored.map(|e| e.entry_version), entry.entry_version) {
            return Ok(InsertOutcome::EntryMoved);
        }
        let outcome = self.insert_row_sync(row)?;
        if outcome != InsertOutcome::Inserted {
            // Roll back by drop: the entry upsert must never land without its
            // journal row (and nothing else was written on this path).
            return Ok(outcome);
        }
        self.conn
            .execute(
                "INSERT INTO state_entries
                     (kind, key, scope, value, merge_meta, entry_version, tombstone)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT (kind, key) DO UPDATE SET
                     scope = excluded.scope,
                     value = excluded.value,
                     merge_meta = excluded.merge_meta,
                     entry_version = excluded.entry_version,
                     tombstone = excluded.tombstone",
                params![
                    entry.kind,
                    entry.key,
                    entry.scope,
                    entry.value,
                    entry.merge_meta,
                    i64::try_from(entry.entry_version).context("entry_version exceeds i64")?,
                    entry.tombstone,
                ],
            )
            .context("state entry upsert")?;
        tx.commit().context("state_put_with_row commit")?;
        Ok(InsertOutcome::Inserted)
    }

    async fn state_get(&self, kind: &str, key: &str) -> Result<Option<StateEntry>> {
        self.conn
            .query_row(
                "SELECT scope, value, merge_meta, entry_version, tombstone
                 FROM state_entries WHERE kind = ?1 AND key = ?2",
                params![kind, key],
                |r| {
                    Ok(StateEntry {
                        kind: kind.to_string(),
                        key: key.to_string(),
                        scope: r.get(0)?,
                        value: r.get(1)?,
                        merge_meta: r.get(2)?,
                        entry_version: r.get::<_, i64>(3)? as u64,
                        tombstone: r.get(4)?,
                    })
                },
            )
            .optional()
            .context("state_entries get")
    }

    async fn group_state_put_with_row(
        &self,
        entry: &StateEntry,
        row: &JournalRow,
        local_writer: Option<&WriterId>,
    ) -> Result<InsertOutcome> {
        let tx = self.write_tx().context("group_state_put_with_row tx")?;
        self.writer_guard_sync(local_writer)?;
        let stored = self
            .group_state_get(&entry.scope, &entry.kind, &entry.key)
            .await?;
        if entry_moved(stored.map(|e| e.entry_version), entry.entry_version) {
            return Ok(InsertOutcome::EntryMoved);
        }
        let outcome = self.insert_row_sync(row)?;
        if outcome != InsertOutcome::Inserted {
            // Roll back by drop: the entry upsert must never land without its
            // journal row (and nothing else was written on this path).
            return Ok(outcome);
        }
        self.conn
            .execute(
                "INSERT INTO group_entries
                     (scope, kind, key, value, merge_meta, entry_version, tombstone)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT (scope, kind, key) DO UPDATE SET
                     value = excluded.value,
                     merge_meta = excluded.merge_meta,
                     entry_version = excluded.entry_version,
                     tombstone = excluded.tombstone",
                params![
                    entry.scope,
                    entry.kind,
                    entry.key,
                    entry.value,
                    entry.merge_meta,
                    i64::try_from(entry.entry_version).context("entry_version exceeds i64")?,
                    entry.tombstone,
                ],
            )
            .context("group entry upsert")?;
        tx.commit().context("group_state_put_with_row commit")?;
        Ok(InsertOutcome::Inserted)
    }

    async fn group_state_get(
        &self,
        scope: &str,
        kind: &str,
        key: &str,
    ) -> Result<Option<StateEntry>> {
        self.conn
            .query_row(
                "SELECT value, merge_meta, entry_version, tombstone
                 FROM group_entries WHERE scope = ?1 AND kind = ?2 AND key = ?3",
                params![scope, kind, key],
                |r| {
                    Ok(StateEntry {
                        kind: kind.to_string(),
                        key: key.to_string(),
                        scope: scope.to_string(),
                        value: r.get(0)?,
                        merge_meta: r.get(1)?,
                        entry_version: r.get::<_, i64>(2)? as u64,
                        tombstone: r.get(3)?,
                    })
                },
            )
            .optional()
            .context("group_entries get")
    }

    async fn group_states_for_scope(&self, scope: &str) -> Result<Vec<StateEntry>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT kind, key, value, merge_meta, entry_version, tombstone
                 FROM group_entries WHERE scope = ?1 ORDER BY kind, key",
            )
            .context("group_entries scope query")?;
        let rows = stmt
            .query_map(params![scope], |r| {
                Ok(StateEntry {
                    kind: r.get(0)?,
                    key: r.get(1)?,
                    scope: scope.to_string(),
                    value: r.get(2)?,
                    merge_meta: r.get(3)?,
                    entry_version: r.get::<_, i64>(4)? as u64,
                    tombstone: r.get(5)?,
                })
            })
            .context("group_entries scope rows")?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .context("group_entries scope collect")
    }

    async fn max_writer_seq(&self, writer: &WriterId) -> Result<Option<u64>> {
        let max: Option<i64> = self.conn.query_row(
            "SELECT MAX(writer_seq) FROM journal WHERE writer_id = ?1",
            params![writer.0.as_slice()],
            |r| r.get(0),
        )?;
        Ok(max.map(|m| m as u64))
    }

    async fn max_scope_writer_seq(&self, scope: &str, writer: &WriterId) -> Result<Option<u64>> {
        let max: Option<i64> = self.conn.query_row(
            "SELECT MAX(writer_seq) FROM journal WHERE scope = ?1 AND writer_id = ?2",
            params![scope, writer.0.as_slice()],
            |r| r.get(0),
        )?;
        Ok(max.map(|m| m as u64))
    }

    async fn coordinate_of_item(
        &self,
        scope: &str,
        item: &ItemRef,
    ) -> Result<Option<(WriterId, u64)>> {
        // MIN(writer_seq) picks the row that introduced the item; the
        // correlated writer_id is that same row's (a scope's rows are the nest
        // sequencer's, so ties across writers do not arise in practice, and
        // ordering by seq then writer keeps the pick deterministic if they
        // ever did).
        let found: Option<(Vec<u8>, i64)> = self
            .conn
            .query_row(
                "SELECT writer_id, writer_seq FROM journal
                 WHERE scope = ?1 AND item_ref = ?2
                 ORDER BY writer_seq ASC, writer_id ASC LIMIT 1",
                params![scope, item.encode()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((writer, seq)) = found else {
            return Ok(None);
        };
        let writer: [u8; 32] = writer
            .try_into()
            .map_err(|_| anyhow::anyhow!("journal writer_id is not 32 bytes"))?;
        Ok(Some((WriterId(writer), seq as u64)))
    }

    async fn rows_for_scope(
        &self,
        scope: &str,
        writer: &WriterId,
        after: u64,
        limit: u32,
    ) -> Result<Vec<JournalRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT writer_id, writer_seq, scope, op, item_ref FROM journal
             WHERE scope = ?1 AND writer_id = ?2 AND writer_seq > ?3
             ORDER BY writer_seq ASC LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            params![
                scope,
                writer.0.as_slice(),
                i64::try_from(after).context("after exceeds i64")?,
                limit
            ],
            |r| {
                Ok((
                    r.get::<_, Vec<u8>>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Vec<u8>>(4)?,
                ))
            },
        )?;
        rows.map(|r| {
            let (w, seq, scope, op, item) = r?;
            Self::row_from_sql(w, seq, scope, op, item)
        })
        .collect()
    }

    async fn frontier(&self, scope: &str) -> Result<Vec<(WriterId, u64)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT writer_id, high_seq FROM frontiers WHERE scope = ?1")?;
        let rows = stmt.query_map(params![scope], |r| {
            Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?))
        })?;
        rows.map(|r| {
            let (w, seq) = r?;
            Ok((
                WriterId(
                    w.as_slice()
                        .try_into()
                        .context("frontier writer_id is not 32 bytes")?,
                ),
                u64::try_from(seq).context("negative high_seq")?,
            ))
        })
        .collect()
    }

    async fn frontier_raise(&self, scope: &str, writer: &WriterId, seq: u64) -> Result<u64> {
        self.conn.execute(
            "INSERT INTO frontiers (scope, writer_id, high_seq) VALUES (?1, ?2, ?3)
             ON CONFLICT (scope, writer_id)
             DO UPDATE SET high_seq = MAX(high_seq, excluded.high_seq)",
            params![
                scope,
                writer.0.as_slice(),
                i64::try_from(seq).context("high_seq exceeds i64")?
            ],
        )?;
        let now: i64 = self.conn.query_row(
            "SELECT high_seq FROM frontiers WHERE scope = ?1 AND writer_id = ?2",
            params![scope, writer.0.as_slice()],
            |r| r.get(0),
        )?;
        u64::try_from(now).context("negative high_seq")
    }

    async fn nest_watermark(&self, scope: &str) -> Result<Option<u64>> {
        self.meta_get_sync(&nest_watermark_key(scope))?
            .map(|raw| ascii_u64(&raw).context("nest watermark"))
            .transpose()
    }

    /// The raise and the read-back share one transaction, so the value
    /// returned is the one this raise left — not a racing raise's.
    async fn nest_watermark_raise(&self, scope: &str, seq: u64) -> Result<u64> {
        let key = nest_watermark_key(scope);
        let tx = self.write_tx().context("nest watermark raise tx")?;
        self.meta_put_u64_max_sync(&key, seq)?;
        let now = self
            .meta_get_sync(&key)?
            .context("nest watermark absent inside its own raise")?;
        tx.commit().context("nest watermark raise commit")?;
        ascii_u64(&now).context("nest watermark")
    }

    async fn nest_watermark_clear(&self, scope: &str) -> Result<()> {
        self.meta_delete(&nest_watermark_key(scope)).await
    }

    async fn relay_high_waters(
        &self,
        scope: &str,
        item_class: &str,
    ) -> Result<Vec<(WriterId, u64)>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT writer_id, MAX(writer_seq) FROM relay_rows
                 WHERE scope = ?1 AND item_class = ?2
                 GROUP BY writer_id",
            )
            .context("prepare relay_high_waters")?;
        let rows = stmt
            .query_map(params![scope, item_class], |r| {
                Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?))
            })
            .context("query relay_high_waters")?;
        rows.map(|r| {
            let (w, seq) = r.context("read relay_high_waters")?;
            let writer = WriterId(
                w.as_slice()
                    .try_into()
                    .context("relay writer_id is not 32 bytes")?,
            );
            Ok((
                writer,
                u64::try_from(seq).context("negative relay writer_seq")?,
            ))
        })
        .collect()
    }

    async fn relay_put(&self, row: &RelayRow) -> Result<()> {
        let writer_seq = i64::try_from(row.writer_seq).context("relay writer_seq exceeds i64")?;
        self.conn
            .execute(
                "INSERT INTO relay_rows
                     (scope, item_class, writer_id, writer_seq, item_key, op, entry, feed_seq,
                      generation_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT (scope, writer_id, item_key)
                 DO UPDATE SET item_class    = excluded.item_class,
                               writer_seq    = excluded.writer_seq,
                               op            = excluded.op,
                               entry         = excluded.entry,
                               feed_seq      = excluded.feed_seq,
                               generation_id = excluded.generation_id
                 WHERE excluded.writer_seq > relay_rows.writer_seq",
                params![
                    row.scope,
                    row.item_class,
                    row.writer.0.as_slice(),
                    writer_seq,
                    row.item_key,
                    row.op,
                    row.entry,
                    row.feed_seq.map(i64::try_from).transpose()?,
                    row.entry
                        .as_deref()
                        .and_then(fauna_core::account_entry_crypto::peek_generation_id)
                        .map(Vec::from),
                ],
            )
            .context("relay_rows put")?;
        // The newer-seq guard above keeps a replay at the SAME coordinates a
        // no-op for the row's bytes; the feed seq is the one column a replay
        // may fill in (the walk's own echo of a row published before the
        // reply carried it, or before the column existed).
        if row.feed_seq.is_some() {
            self.relay_stamp_feed_seq(
                &row.scope,
                &row.writer,
                &row.item_key,
                row.writer_seq,
                row.feed_seq.unwrap_or_default(),
            )
            .await?;
        }
        Ok(())
    }

    async fn relay_stamp_feed_seq(
        &self,
        scope: &str,
        writer: &WriterId,
        item_key: &[u8],
        writer_seq: u64,
        feed_seq: u64,
    ) -> Result<()> {
        self.conn
            .execute(
                "UPDATE relay_rows SET feed_seq = ?5
                 WHERE scope = ?1 AND writer_id = ?2 AND item_key = ?3 AND writer_seq = ?4",
                params![
                    scope,
                    writer.0.as_slice(),
                    item_key,
                    i64::try_from(writer_seq).context("relay writer_seq exceeds i64")?,
                    i64::try_from(feed_seq).context("relay feed_seq exceeds i64")?,
                ],
            )
            .context("relay_rows feed_seq stamp")?;
        Ok(())
    }

    async fn relay_clear_feed_seqs(&self, scope: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE relay_rows SET feed_seq = NULL
                 WHERE scope = ?1 AND feed_seq IS NOT NULL",
                params![scope],
            )
            .context("relay_rows feed_seq clear")?;
        Ok(())
    }

    async fn issued_retire_put(&self, retire: &IssuedRetire, cap: u32) -> Result<()> {
        let tx = self.write_tx().context("issued_retires put tx")?;
        // Delete-then-insert, never an upsert: the unique order index would
        // otherwise have to move a held token under its own constraint.
        tx.execute(
            "DELETE FROM issued_retires
             WHERE scope = ?1 AND writer_id = ?2 AND item_key = ?3 AND writer_seq = ?4",
            params![
                retire.scope,
                retire.writer.0.as_slice(),
                retire.item_key.as_slice(),
                i64::try_from(retire.writer_seq).context("retire writer_seq exceeds i64")?,
            ],
        )
        .context("issued_retires replace")?;
        tx.execute(
            "INSERT INTO issued_retires
                 (scope, writer_id, item_key, writer_seq, ord, belt, escrow_sweep, settled)
             VALUES (?1, ?2, ?3, ?4,
                     (SELECT COALESCE(MAX(ord), 0) + 1 FROM issued_retires),
                     ?5, ?6, ?7)",
            params![
                retire.scope,
                retire.writer.0.as_slice(),
                retire.item_key.as_slice(),
                i64::try_from(retire.writer_seq).context("retire writer_seq exceeds i64")?,
                retire.no_rows_sealed_under.as_ref().map(|g| g.as_slice()),
                retire.delete_escrow_wraps,
                retire.settled,
            ],
        )
        .context("issued_retires put")?;
        // Bounded, newest kept: everything older than the newest `cap` goes.
        tx.execute(
            "DELETE FROM issued_retires
             WHERE ord <= (SELECT ord FROM issued_retires
                           ORDER BY ord DESC LIMIT 1 OFFSET ?1)",
            params![i64::from(cap)],
        )
        .context("issued_retires trim")?;
        tx.commit().context("issued_retires put commit")
    }

    async fn issued_retires(&self) -> Result<Vec<(u64, IssuedRetire)>> {
        let mut stmt = self.conn.prepare(
            "SELECT ord, scope, writer_id, item_key, writer_seq, belt, escrow_sweep, settled
             FROM issued_retires ORDER BY ord ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Vec<u8>>(2)?,
                r.get::<_, Vec<u8>>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, Option<Vec<u8>>>(5)?,
                r.get::<_, bool>(6)?,
                r.get::<_, bool>(7)?,
            ))
        })?;
        let id32 = |what: &str, b: Vec<u8>| -> Result<[u8; 32]> {
            b.try_into()
                .map_err(|b: Vec<u8>| anyhow::anyhow!("retire {what} is {} bytes", b.len()))
        };
        let mut out = Vec::new();
        for r in rows {
            let (ord, scope, writer, item_key, seq, belt, delete_escrow_wraps, settled) = r?;
            out.push((
                u64::try_from(ord).context("negative retire order token")?,
                IssuedRetire {
                    scope,
                    writer: WriterId(id32("writer", writer)?),
                    item_key: id32("item key", item_key)?,
                    writer_seq: u64::try_from(seq).context("negative retire writer_seq")?,
                    no_rows_sealed_under: belt.map(|b| id32("belt", b)).transpose()?,
                    delete_escrow_wraps,
                    settled,
                },
            ));
        }
        Ok(out)
    }

    async fn issued_retires_clear_through(&self, through: u64) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM issued_retires WHERE ord <= ?1",
                params![i64::try_from(through).context("retire order token exceeds i64")?],
            )
            .context("issued_retires clear")?;
        Ok(())
    }

    async fn relay_retire_shadowed(
        &self,
        scope: &str,
        writer: &WriterId,
        writer_seq: u64,
        keep_item_key: &[u8],
    ) -> Result<u64> {
        let deleted = delete_relay_rows_at(
            &self.conn,
            scope,
            writer,
            i64::try_from(writer_seq).context("relay writer_seq exceeds i64")?,
            Some(keep_item_key),
        )
        .context("relay_rows retire shadowed")?;
        Ok(deleted as u64)
    }

    async fn relay_retire_at(
        &self,
        scope: &str,
        writer: &WriterId,
        writer_seq: u64,
    ) -> Result<u64> {
        let deleted = delete_relay_rows_at(
            &self.conn,
            scope,
            writer,
            i64::try_from(writer_seq).context("relay writer_seq exceeds i64")?,
            None,
        )
        .context("relay_rows retire at coordinate")?;
        Ok(deleted as u64)
    }

    async fn relay_rows(
        &self,
        scope: &str,
        item_class: &str,
        frontier: &[(WriterId, u64)],
        limit: u32,
    ) -> Result<Vec<RelayRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT item_class, writer_id, writer_seq, item_key, op, entry, feed_seq
             FROM relay_rows WHERE scope = ?1 AND item_class = ?2
             ORDER BY writer_id ASC, writer_seq ASC",
        )?;
        let rows = stmt.query_map(params![scope, item_class], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Vec<u8>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<Vec<u8>>>(5)?,
                r.get::<_, Option<i64>>(6)?,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (item_class, w, seq, item_key, op, entry, feed_seq) = r?;
            let writer = WriterId(
                w.as_slice()
                    .try_into()
                    .context("relay writer_id is not 32 bytes")?,
            );
            let writer_seq = u64::try_from(seq).context("negative relay writer_seq")?;
            // Frontier gating in the walk loop rather than SQL: the vector is
            // a per-writer map, and a hand-built dynamic IN-clause would be
            // exactly the drift-prone SQL the nest's own serve avoids.
            let high = frontier
                .iter()
                .find(|(fw, _)| *fw == writer)
                .map(|(_, s)| *s)
                .unwrap_or(0);
            if writer_seq <= high {
                continue;
            }
            out.push(RelayRow {
                scope: scope.to_string(),
                item_class,
                writer,
                writer_seq,
                item_key,
                op,
                entry,
                feed_seq: relay_feed_seq(feed_seq)?,
            });
            if out.len() as u32 >= limit {
                break;
            }
        }
        Ok(out)
    }

    async fn state_forget(&self, kind: &str, key: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM state_entries WHERE kind = ?1 AND key = ?2",
            params![kind, key],
        )?;
        Ok(())
    }

    async fn relay_forget(&self, scope: &str, writer: &WriterId, item_key: &[u8]) -> Result<()> {
        self.conn.execute(
            "DELETE FROM relay_rows WHERE scope = ?1 AND writer_id = ?2 AND item_key = ?3",
            params![scope, writer.0.as_slice(), item_key],
        )?;
        Ok(())
    }

    async fn relay_generations(&self, scope: &str) -> Result<Vec<[u8; 32]>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT generation_id FROM relay_rows
             WHERE scope = ?1 AND generation_id IS NOT NULL",
        )?;
        let rows = stmt.query_map(params![scope], |r| r.get::<_, Vec<u8>>(0))?;
        let mut out = Vec::new();
        for r in rows {
            // A malformed stamp cannot come from `relay_put` (it stores a
            // header's 32 bytes or nothing); skipped, never an error.
            if let Ok(g) = <[u8; 32]>::try_from(r?.as_slice()) {
                out.push(g);
            }
        }
        Ok(out)
    }

    async fn relay_forget_sealed_under(&self, scope: &str, generation: &[u8; 32]) -> Result<u64> {
        let n = self
            .conn
            .execute(
                "DELETE FROM relay_rows WHERE scope = ?1 AND generation_id = ?2",
                params![scope, generation.as_slice()],
            )
            .context("relay_rows forget sealed under")?;
        Ok(n as u64)
    }

    async fn relay_rows_sealed_under(
        &self,
        scope: &str,
        generation: &[u8; 32],
    ) -> Result<Vec<RelayRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT item_class, writer_id, writer_seq, item_key, op, entry, feed_seq
             FROM relay_rows WHERE scope = ?1 AND generation_id = ?2",
        )?;
        let rows = stmt.query_map(params![scope, generation.as_slice()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Vec<u8>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<Vec<u8>>>(5)?,
                r.get::<_, Option<i64>>(6)?,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (item_class, w, seq, item_key, op, entry, feed_seq) = r?;
            out.push(RelayRow {
                scope: scope.to_string(),
                item_class,
                writer: WriterId(
                    w.as_slice()
                        .try_into()
                        .context("relay writer_id is not 32 bytes")?,
                ),
                writer_seq: u64::try_from(seq).context("negative relay writer_seq")?,
                item_key,
                op,
                entry,
                feed_seq: relay_feed_seq(feed_seq)?,
            });
        }
        Ok(out)
    }

    async fn relay_rows_at(
        &self,
        scope: &str,
        item_class: &str,
        item_key: &[u8],
    ) -> Result<Vec<RelayRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT writer_id, writer_seq, op, entry, feed_seq
             FROM relay_rows WHERE scope = ?1 AND item_class = ?2 AND item_key = ?3
             ORDER BY writer_id ASC",
        )?;
        let rows = stmt.query_map(params![scope, item_class, item_key], |r| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<Vec<u8>>>(3)?,
                r.get::<_, Option<i64>>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (w, seq, op, entry, feed_seq) = r?;
            out.push(RelayRow {
                scope: scope.to_string(),
                item_class: item_class.to_string(),
                writer: WriterId(
                    w.as_slice()
                        .try_into()
                        .context("relay writer_id is not 32 bytes")?,
                ),
                writer_seq: u64::try_from(seq).context("negative relay writer_seq")?,
                item_key: item_key.to_vec(),
                op,
                entry,
                feed_seq: relay_feed_seq(feed_seq)?,
            });
        }
        Ok(out)
    }

    async fn relay_rows_of_writer(
        &self,
        scope: &str,
        item_class: &str,
        writer: &WriterId,
    ) -> Result<Vec<RelayRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT writer_seq, item_key, op, entry, feed_seq
             FROM relay_rows WHERE scope = ?1 AND item_class = ?2 AND writer_id = ?3
             ORDER BY writer_seq ASC",
        )?;
        let rows = stmt.query_map(params![scope, item_class, writer.0.as_slice()], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<Vec<u8>>>(3)?,
                r.get::<_, Option<i64>>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (seq, item_key, op, entry, feed_seq) = r?;
            out.push(RelayRow {
                scope: scope.to_string(),
                item_class: item_class.to_string(),
                writer: *writer,
                writer_seq: u64::try_from(seq).context("negative relay writer_seq")?,
                item_key,
                op,
                entry,
                feed_seq: relay_feed_seq(feed_seq)?,
            });
        }
        Ok(out)
    }

    async fn relay_meter(&self, floor_ops: &[&str]) -> Result<Vec<RelayScopeMeter>> {
        // The floor test is done in Rust rather than a hand-built dynamic
        // IN-clause, for the same reason `relay_rows` gates the frontier in the
        // walk loop: a generated SQL fragment over a caller-supplied list is
        // exactly the drift-prone shape this backend avoids.
        let mut stmt = self.conn.prepare(
            "SELECT scope, item_class, op, length(entry)
             FROM relay_rows
             ORDER BY scope ASC, item_class ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<i64>>(3)?,
            ))
        })?;
        let mut out: Vec<RelayScopeMeter> = Vec::new();
        for r in rows {
            let (scope, item_class, op, len) = r?;
            let bytes = u64::try_from(len.unwrap_or(0)).unwrap_or(0);
            let evictable = !floor_ops.contains(&op.as_str()) && bytes > 0;
            match out
                .last_mut()
                .filter(|m| m.scope == scope && m.item_class == item_class)
            {
                Some(m) => {
                    m.rows += 1;
                    m.payload_bytes = m.payload_bytes.saturating_add(bytes);
                    if evictable {
                        m.evictable_rows += 1;
                        m.evictable_bytes = m.evictable_bytes.saturating_add(bytes);
                    }
                }
                None => out.push(RelayScopeMeter {
                    scope,
                    item_class,
                    rows: 1,
                    payload_bytes: bytes,
                    evictable_rows: u64::from(evictable),
                    evictable_bytes: if evictable { bytes } else { 0 },
                }),
            }
        }
        Ok(out)
    }

    async fn relay_evict_payload(
        &self,
        scope: &str,
        item_class: &str,
        target_bytes: u64,
        floor_ops: &[&str],
    ) -> Result<RelayEvicted> {
        if target_bytes == 0 {
            return Ok(RelayEvicted::default());
        }
        // Oldest first: the rows most likely already superseded at the owner,
        // and the least likely to be the tip a restore reaches for. T15 leaves
        // the ordering heuristic to the build.
        let victims: Vec<(Vec<u8>, Vec<u8>, u64)> = {
            let mut stmt = self.conn.prepare(
                "SELECT writer_id, item_key, op, length(entry)
                 FROM relay_rows
                 WHERE scope = ?1 AND item_class = ?2 AND entry IS NOT NULL
                 ORDER BY writer_seq ASC, writer_id ASC",
            )?;
            let rows = stmt.query_map(params![scope, item_class], |r| {
                Ok((
                    r.get::<_, Vec<u8>>(0)?,
                    r.get::<_, Vec<u8>>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?;
            let mut picked = Vec::new();
            let mut running = 0u64;
            for r in rows {
                let (writer_id, item_key, op, len) = r?;
                if floor_ops.contains(&op.as_str()) {
                    continue;
                }
                let bytes = u64::try_from(len).unwrap_or(0);
                picked.push((writer_id, item_key, bytes));
                running = running.saturating_add(bytes);
                if running >= target_bytes {
                    break;
                }
            }
            picked
        };

        let mut freed = RelayEvicted::default();
        for (writer_id, item_key, bytes) in victims {
            // Coordinates untouched — only the payload column is cleared, so
            // the row keeps serving its shape and the frontier still accounts
            // it. Dehydration, not deletion.
            let n = self.conn.execute(
                "UPDATE relay_rows SET entry = NULL
                 WHERE scope = ?1 AND writer_id = ?2 AND item_key = ?3
                   AND entry IS NOT NULL",
                params![scope, writer_id, item_key],
            )?;
            if n > 0 {
                freed.rows += 1;
                freed.bytes = freed.bytes.saturating_add(bytes);
            }
        }
        Ok(freed)
    }

    async fn state_entries_of_kind(&self, kind: &str) -> Result<Vec<StateEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT key, scope, value, merge_meta, entry_version, tombstone
             FROM state_entries WHERE kind = ?1 AND tombstone = 0
             ORDER BY key ASC",
        )?;
        let rows = stmt.query_map(params![kind], |r| {
            Ok(StateEntry {
                kind: kind.to_string(),
                key: r.get(0)?,
                scope: r.get(1)?,
                value: r.get(2)?,
                merge_meta: r.get(3)?,
                entry_version: r.get::<_, i64>(4)? as u64,
                tombstone: r.get(5)?,
            })
        })?;
        rows.map(|r| r.context("state_entries_of_kind row"))
            .collect()
    }

    async fn block_put(&self, cid: &ContentHash, bytes: &[u8]) -> Result<()> {
        self.block_put_sync(cid, bytes)
    }

    async fn block_get(&self, cid: &ContentHash) -> Result<Option<Vec<u8>>> {
        let loose: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT bytes FROM blocks WHERE cid = ?1",
                params![cid.as_bytes().as_slice()],
                |r| r.get(0),
            )
            .optional()
            .context("blocks get")?;
        if loose.is_some() {
            return Ok(loose);
        }
        // The other placement. Route through `segment_blocks`, then read the
        // block out of the segment's own CARv2 index — the file stays the one
        // authority on where its bytes sit.
        let Some(key) = self.segment_of_block_sync(cid)? else {
            return Ok(None);
        };
        let (dat_path, _) = self.segment_paths(&key)?;
        let file = std::fs::File::open(&dat_path)
            .with_context(|| format!("open adopted segment {}", dat_path.display()))?;
        let mut reader = fauna_carv2::Reader::new(file)
            .map_err(|e| anyhow::anyhow!("adopted segment {}: {e}", dat_path.display()))?;
        match reader.get(cid) {
            Ok(bytes) => Ok(Some(bytes)),
            // The routing row says this segment holds it; the file disagreeing
            // is corruption, not absence — surface it rather than reporting a
            // dehydrated block.
            Err(e) => Err(anyhow::anyhow!(
                "adopted segment {} does not yield the block its routing row claims: {e}",
                dat_path.display()
            )),
        }
    }

    async fn block_has(&self, cid: &ContentHash) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT
                 (SELECT COUNT(*) FROM blocks WHERE cid = ?1)
               + (SELECT COUNT(*) FROM segment_blocks WHERE cid = ?1)",
            params![cid.as_bytes().as_slice()],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    async fn block_delete(&self, cid: &ContentHash) -> Result<bool> {
        let n = self.conn.execute(
            "DELETE FROM blocks WHERE cid = ?1",
            params![cid.as_bytes().as_slice()],
        )?;
        Ok(n > 0)
    }

    async fn record_index_put(&self, entry: &RecordIndexEntry) -> Result<()> {
        self.record_index_put_sync(entry)
    }

    async fn record_index_get(&self, cid: &ContentHash) -> Result<Option<RecordIndexEntry>> {
        self.conn
            .query_row(
                "SELECT scope, kind, size FROM record_index WHERE cid = ?1",
                params![cid.as_bytes().as_slice()],
                |r| {
                    Ok(RecordIndexEntry {
                        cid: *cid,
                        scope: r.get(0)?,
                        kind: r.get(1)?,
                        size: r.get::<_, Option<i64>>(2)?.map(|s| s as u64),
                    })
                },
            )
            .optional()
            .context("record_index get")
    }

    async fn record_index_delete(&self, cid: &ContentHash) -> Result<bool> {
        let n = self
            .conn
            .execute(
                "DELETE FROM record_index WHERE cid = ?1",
                params![cid.as_bytes().as_slice()],
            )
            .context("record_index delete")?;
        Ok(n > 0)
    }

    async fn records_in_scope(
        &self,
        scope: &str,
        after: Option<&ContentHash>,
        limit: u32,
    ) -> Result<Vec<RecordIndexEntry>> {
        // An all-zero low bound: CIDs are fixed-width, so BLOB comparison
        // orders them totally and `> x''` admits every row.
        let after_bytes: Vec<u8> = after.map_or_else(Vec::new, |c| c.as_bytes().to_vec());
        let mut stmt = self.conn.prepare(
            "SELECT cid, scope, kind, size FROM record_index
             WHERE scope = ?1 AND cid > ?2
             ORDER BY cid ASC LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![scope, after_bytes, limit], |r| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<i64>>(3)?,
            ))
        })?;
        rows.map(|r| {
            let (cid, scope, kind, size) = r?;
            let cid: [u8; 36] = cid
                .as_slice()
                .try_into()
                .context("record_index cid is not 36 bytes")?;
            Ok(RecordIndexEntry {
                cid: ContentHash::from_bytes(cid)?,
                scope,
                kind,
                size: size.map(|s| s as u64),
            })
        })
        .collect()
    }

    async fn record_added_with_row(
        &self,
        entry: &RecordIndexEntry,
        bytes: Option<&[u8]>,
        row: &JournalRow,
        local_writer: Option<&WriterId>,
    ) -> Result<InsertOutcome> {
        let tx = self.write_tx().context("record_added_with_row tx")?;
        self.writer_guard_sync(local_writer)?;
        let outcome = self.insert_row_sync(row)?;
        if outcome != InsertOutcome::Inserted {
            // Roll back by drop — same law as `state_put_with_row`: the index
            // row and the bytes must never land without their journal row.
            return Ok(outcome);
        }
        self.record_index_put_sync(entry)?;
        if let Some(bytes) = bytes {
            self.block_put_sync(&entry.cid, bytes)?;
        }
        tx.commit().context("record_added_with_row commit")?;
        Ok(InsertOutcome::Inserted)
    }

    async fn segment_stage(&self) -> Result<SqliteStaging> {
        let Some(dir) = self.segment_dir.as_ref() else {
            bail!(
                "this store has no segment area (in-memory backend) — \
                 adopt segments against a store opened on a directory"
            );
        };
        std::fs::create_dir_all(dir)
            .with_context(|| format!("create segment area {}", dir.display()))?;
        SqliteStaging::open(dir)
    }

    async fn segment_adopt(
        &self,
        key: &SegmentKey,
        mut staged: SqliteStaging,
        blocks: &[AdoptedBlock],
    ) -> Result<bool> {
        let (dat_path, meta_path) = self.segment_paths(key)?;
        let already: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM segments WHERE scope = ?1 AND kind = ?2 AND segment_id = ?3",
            params![key.scope, key.kind, key.segment_id],
            |r| r.get(0),
        )?;
        if already > 0 {
            return Ok(false);
        }

        // Files first, rows second: a file with no row is invisible (the next
        // adoption overwrites it), while a row with no file is a routing
        // entry pointing at nothing — the failure `block_get` cannot recover
        // from. Both land verbatim, by rename; nothing re-encodes either.
        let meta = staged.meta().await?;
        let dat_len = staged.len(SegmentHalf::Dat);
        staged.install(&dat_path, &meta_path)?;

        let tx = self.write_tx().context("adopt tx")?;
        self.conn
            .execute(
                "INSERT INTO segments (scope, kind, segment_id, meta, dat_len)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    key.scope,
                    key.kind,
                    key.segment_id,
                    meta.as_slice(),
                    i64::try_from(dat_len).context("dat len exceeds i64")?,
                ],
            )
            .context("segments insert")?;
        for block in blocks {
            self.conn
                .execute(
                    "INSERT OR IGNORE INTO segment_blocks (cid, scope, kind, segment_id, len)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        block.cid.as_bytes().as_slice(),
                        key.scope,
                        key.kind,
                        key.segment_id,
                        i64::try_from(block.len).context("block len exceeds i64")?,
                    ],
                )
                .context("segment_blocks insert")?;
        }
        tx.commit().context("adopt commit")?;
        Ok(true)
    }

    async fn segment_meter(&self) -> Result<Vec<SegmentScopeMeter>> {
        let mut stmt = self.conn.prepare(
            "SELECT scope, COUNT(*), COUNT(dat_len), COALESCE(SUM(dat_len), 0),
                    COALESCE(SUM(length(meta)), 0)
             FROM segments GROUP BY scope ORDER BY scope ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?;
        rows.map(|r| {
            let (scope, segments, held, dat, meta) = r?;
            Ok(SegmentScopeMeter {
                scope,
                segments: u64::try_from(segments).unwrap_or(0),
                held_segments: u64::try_from(held).unwrap_or(0),
                dat_bytes: u64::try_from(dat).unwrap_or(0),
                meta_bytes: u64::try_from(meta).unwrap_or(0),
            })
        })
        .collect()
    }

    async fn segment_evict_dat(&self, scope: &str, target_bytes: u64) -> Result<RelayEvicted> {
        if target_bytes == 0 {
            return Ok(RelayEvicted::default());
        }
        let victims: Vec<(String, i64, u64)> = {
            let mut stmt = self.conn.prepare(
                "SELECT kind, segment_id, dat_len FROM segments
                 WHERE scope = ?1 AND dat_len IS NOT NULL
                 ORDER BY segment_id ASC, kind ASC",
            )?;
            let rows = stmt.query_map(params![scope], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?;
            let mut picked = Vec::new();
            let mut running = 0u64;
            for r in rows {
                let (kind, segment_id, len) = r?;
                let bytes = u64::try_from(len).unwrap_or(0);
                picked.push((kind, segment_id, bytes));
                running = running.saturating_add(bytes);
                if running >= target_bytes {
                    break;
                }
            }
            picked
        };

        let mut freed = RelayEvicted::default();
        for (kind, segment_id, bytes) in victims {
            let tx = self.write_tx().context("evict tx")?;
            self.conn
                .execute(
                    "DELETE FROM segment_blocks
                     WHERE scope = ?1 AND kind = ?2 AND segment_id = ?3",
                    params![scope, kind, segment_id],
                )
                .context("segment_blocks evict")?;
            let n = self
                .conn
                .execute(
                    "UPDATE segments SET dat_len = NULL
                     WHERE scope = ?1 AND kind = ?2 AND segment_id = ?3
                       AND dat_len IS NOT NULL",
                    params![scope, kind, segment_id],
                )
                .context("segments evict")?;
            tx.commit().context("evict commit")?;
            if n == 0 {
                continue;
            }
            let key = SegmentKey {
                scope: scope.to_string(),
                kind,
                segment_id: u32::try_from(segment_id).context("segment_id out of range")?,
            };
            let (dat_path, _) = self.segment_paths(&key)?;
            match std::fs::remove_file(&dat_path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(e)
                        .with_context(|| format!("remove evicted segment {}", dat_path.display()));
                }
            }
            freed.rows += 1;
            freed.bytes = freed.bytes.saturating_add(bytes);
        }
        Ok(freed)
    }

    async fn segments_in_scope(&self, scope: &str) -> Result<Vec<SegmentKey>> {
        let mut stmt = self.conn.prepare(
            "SELECT kind, segment_id FROM segments WHERE scope = ?1
             ORDER BY kind ASC, segment_id ASC",
        )?;
        let rows = stmt.query_map(params![scope], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        rows.map(|r| {
            let (kind, segment_id) = r?;
            Ok(SegmentKey {
                scope: scope.to_string(),
                kind,
                segment_id: u32::try_from(segment_id).context("segment_id out of range")?,
            })
        })
        .collect()
    }

    async fn segment_meta(&self, key: &SegmentKey) -> Result<Option<Vec<u8>>> {
        self.conn
            .query_row(
                "SELECT meta FROM segments WHERE scope = ?1 AND kind = ?2 AND segment_id = ?3",
                params![key.scope, key.kind, key.segment_id],
                |r| r.get(0),
            )
            .optional()
            .context("segments meta get")
    }

    async fn segment_of_block(&self, cid: &ContentHash) -> Result<Option<SegmentKey>> {
        self.segment_of_block_sync(cid)
    }

    async fn segment_block_len(&self, cid: &ContentHash) -> Result<Option<u64>> {
        let len: Option<i64> = self
            .conn
            .query_row(
                "SELECT len FROM segment_blocks WHERE cid = ?1 LIMIT 1",
                params![cid.as_bytes().as_slice()],
                |r| r.get(0),
            )
            .optional()
            .context("segment_blocks len")?;
        len.map(|l| u64::try_from(l).context("negative block len"))
            .transpose()
    }

    /// Rows first, files second — the inverse of [`Self::segment_adopt`]'s
    /// order, and for the same reason. A segment file with no row is invisible
    /// (nothing routes to it, the next adoption of that key overwrites it),
    /// while a row with no file is the failure `block_get` cannot recover
    /// from. So the crash window this leaves is the harmless one, and the
    /// pending mark closes even that at the next open.
    async fn drop_scope(&self, scope: &str) -> Result<ScopeDropCounts> {
        let counts = self.drop_scope_rows(scope)?;
        self.sweep_scope_segment_files(scope)?;
        self.clear_pending_scope_drop(scope)?;
        Ok(counts)
    }

    async fn scopes_with_frontiers(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT scope FROM frontiers ORDER BY scope")
            .context("prepare frontier scopes")?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .context("query frontier scopes")?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("read frontier scopes")
    }

    // ── The outbox (W4) ──────────────────────────────────────────────────────

    async fn outbox_append(
        &self,
        intent: &NewOutboxIntent,
        local_writer: Option<&WriterId>,
    ) -> Result<bool> {
        // The guard rides its own transaction with the insert attempt so a
        // stale process cannot enqueue an intent behind the succession fence
        // (the intent payload may embed the writer identity).
        let tx = self.write_tx().context("outbox append tx")?;
        self.writer_guard_sync(local_writer)?;
        // OR IGNORE folds BOTH possible conflicts into "0 rows"; the journal's
        // `insert_row_sync` disambiguation pattern applies: an existing
        // intent_id is the composer's crash-retry (idempotent no-op, `false`),
        // while a (scope, channel_seq) collision is another process winning
        // the seq race — retry with a fresh MAX, bounded. A silent drop here
        // would lose the only copy of a pending write, so the loop's
        // exhaustion is an error, never `false`.
        for _ in 0..8 {
            let inserted = self
                .conn
                .execute(
                    "INSERT OR IGNORE INTO outbox
                         (intent_id, kind, scope, payload, drainer, channel_seq, created_at)
                     SELECT ?1, ?2, ?3, ?4, ?5, COALESCE(MAX(channel_seq), 0) + 1, ?6
                     FROM outbox WHERE scope = ?3",
                    params![
                        intent.intent_id.as_slice(),
                        intent.kind,
                        intent.scope,
                        intent.payload,
                        intent.drainer.as_str(),
                        now_epoch_secs(),
                    ],
                )
                .context("outbox append")?;
            if inserted > 0 {
                tx.commit().context("outbox append commit")?;
                return Ok(true);
            }
            let present: bool = self
                .conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM outbox WHERE intent_id = ?1)",
                    params![intent.intent_id.as_slice()],
                    |r| r.get(0),
                )
                .context("outbox re-append check")?;
            if present {
                // Nothing was written on this path — commit-vs-drop is
                // equivalent; commit keeps the no-rollback invariant obvious.
                tx.commit().context("outbox append commit")?;
                return Ok(false);
            }
        }
        bail!(
            "outbox append: lost the per-scope seq race 8 times (scope {:?})",
            intent.scope
        )
    }

    async fn outbox_undrained(&self) -> Result<Vec<OutboxIntent>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT intent_id, kind, scope, payload, drainer, channel_seq,
                        status, retry_count, created_at, last_attempt_at
                 FROM outbox ORDER BY scope, channel_seq",
            )
            .context("prepare outbox list")?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, Vec<u8>>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Vec<u8>>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, u64>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, u32>(7)?,
                    r.get::<_, u64>(8)?,
                    r.get::<_, Option<u64>>(9)?,
                ))
            })
            .context("query outbox")?;
        let mut out = Vec::new();
        for row in rows {
            let (id, kind, scope, payload, drainer, channel_seq, status, retries, created, last) =
                row.context("read outbox row")?;
            let intent_id: [u8; 16] = id
                .try_into()
                .map_err(|v: Vec<u8>| anyhow::anyhow!("outbox intent_id: {} bytes", v.len()))?;
            out.push(OutboxIntent {
                intent_id,
                kind,
                scope,
                payload,
                drainer: IntentDrainer::parse(&drainer)?,
                channel_seq,
                status: IntentStatus::parse(&status)?,
                retry_count: retries,
                created_at: created,
                last_attempt_at: last,
            });
        }
        Ok(out)
    }

    async fn outbox_ack(&self, intent_id: &[u8; 16]) -> Result<bool> {
        let deleted = self
            .conn
            .execute(
                "DELETE FROM outbox WHERE intent_id = ?1",
                params![intent_id.as_slice()],
            )
            .context("outbox ack")?;
        Ok(deleted > 0)
    }

    async fn outbox_mark_failed(&self, intent_id: &[u8; 16]) -> Result<bool> {
        let updated = self
            .conn
            .execute(
                "UPDATE outbox SET status = 'failed' WHERE intent_id = ?1",
                params![intent_id.as_slice()],
            )
            .context("outbox mark failed")?;
        Ok(updated > 0)
    }

    async fn outbox_record_attempt(&self, intent_id: &[u8; 16]) -> Result<bool> {
        let updated = self
            .conn
            .execute(
                "UPDATE outbox
                 SET retry_count = retry_count + 1, last_attempt_at = ?2
                 WHERE intent_id = ?1",
                params![intent_id.as_slice(), now_epoch_secs()],
            )
            .context("outbox record attempt")?;
        Ok(updated > 0)
    }
}

/// Seconds since epoch — the outbox's `created_at`/`last_attempt_at` stamps
/// (same resolution as `SyncDb`'s `transfer_queue`).
fn now_epoch_secs() -> i64 {
    fauna_core::data::Timestamp::now_secs_or_zero()
}

/// The SQLite arm's [`SegmentStaging`] slot: the pair's two files in the
/// segment area under [`STAGING_PREFIX`], each held open under an **exclusive
/// advisory lock** for the slot's life (the [`crate::locks`] idiom — the
/// kernel releases it when the holder dies). That lock is what keeps the
/// open-time sweep ([`SqliteBackend::sweep_staged_segments`]) to a dead
/// process's leftovers: a store dir is shared by every same-account process
/// (`account-runtime.md` § Multi-instance concurrency), and one opening while
/// another transfers must not delete the transfer's files.
pub struct SqliteStaging {
    dat: StagedFile,
    meta: StagedFile,
    /// Set once the files were renamed into place (or abandoned as a crash):
    /// the drop then leaves the paths alone.
    settled: bool,
}

struct StagedFile {
    path: PathBuf,
    /// `None` once closed — before a rename or a read-back, since Windows
    /// refuses both on a file this process holds open for writing.
    file: Option<std::io::BufWriter<std::fs::File>>,
    len: u64,
}

impl StagedFile {
    fn create(path: PathBuf) -> Result<Self> {
        let file = std::fs::File::options()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| format!("create staging file {}", path.display()))?;
        file.lock()
            .with_context(|| format!("lock staging file {}", path.display()))?;
        Ok(Self {
            path,
            file: Some(std::io::BufWriter::with_capacity(
                SEGMENT_TRANSFER_CHUNK,
                file,
            )),
            len: 0,
        })
    }

    fn write(&mut self, chunk: &[u8]) -> Result<()> {
        use std::io::Write;
        let Some(file) = self.file.as_mut() else {
            bail!("staging file {} is already closed", self.path.display());
        };
        file.write_all(chunk)
            .with_context(|| format!("write staging file {}", self.path.display()))?;
        self.len += chunk.len() as u64;
        Ok(())
    }

    /// Flush and close (which releases the lock). Idempotent.
    fn close(&mut self) -> Result<()> {
        if let Some(file) = self.file.take() {
            let file = file
                .into_inner()
                .map_err(|e| e.into_error())
                .with_context(|| format!("flush staging file {}", self.path.display()))?;
            file.sync_all()
                .with_context(|| format!("sync staging file {}", self.path.display()))?;
        }
        Ok(())
    }
}

impl SqliteStaging {
    fn open(dir: &Path) -> Result<Self> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let stem = staging_stem(&format!("{}-{n}", std::process::id()));
        Ok(Self {
            dat: StagedFile::create(dir.join(format!("{stem}.dat")))?,
            meta: StagedFile::create(dir.join(format!("{stem}.meta")))?,
            settled: false,
        })
    }

    fn half(&mut self, half: SegmentHalf) -> &mut StagedFile {
        match half {
            SegmentHalf::Dat => &mut self.dat,
            SegmentHalf::Meta => &mut self.meta,
        }
    }

    /// Rename both files onto the adopted pair's paths — `.dat` first, so a
    /// crash between the renames leaves a `.dat` no row routes (the next
    /// adoption of that key replaces it) and a staging `.meta` the next open
    /// sweeps.
    fn install(&mut self, dat_path: &Path, meta_path: &Path) -> Result<()> {
        self.dat.close()?;
        self.meta.close()?;
        std::fs::rename(&self.dat.path, dat_path).with_context(|| {
            format!(
                "rename {} -> {}",
                self.dat.path.display(),
                dat_path.display()
            )
        })?;
        std::fs::rename(&self.meta.path, meta_path).with_context(|| {
            format!(
                "rename {} -> {}",
                self.meta.path.display(),
                meta_path.display()
            )
        })?;
        self.settled = true;
        Ok(())
    }
}

impl SegmentSink for SqliteStaging {
    async fn write(&mut self, half: SegmentHalf, chunk: &[u8]) -> Result<()> {
        self.half(half).write(chunk)
    }
}

impl SegmentStaging for SqliteStaging {
    fn len(&self, half: SegmentHalf) -> u64 {
        match half {
            SegmentHalf::Dat => self.dat.len,
            SegmentHalf::Meta => self.meta.len,
        }
    }

    async fn meta(&mut self) -> Result<Vec<u8>> {
        self.meta.close()?;
        std::fs::read(&self.meta.path)
            .with_context(|| format!("read staged {}", self.meta.path.display()))
    }

    async fn dat_reader(&mut self) -> Result<Box<dyn ReadSeek + '_>> {
        self.dat.close()?;
        let file = std::fs::File::open(&self.dat.path)
            .with_context(|| format!("open staged {}", self.dat.path.display()))?;
        Ok(Box::new(std::io::BufReader::with_capacity(
            SEGMENT_TRANSFER_CHUNK,
            file,
        )))
    }

    #[cfg(any(test, feature = "test-helpers"))]
    async fn abandon_as_crash(mut self) {
        self.dat.close().expect("close staged .dat");
        self.meta.close().expect("close staged .meta");
        self.settled = true;
    }
}

impl Drop for SqliteStaging {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        // Discarded un-adopted: take the bytes with it. Best effort — a file
        // this cannot remove is swept at the next open.
        for staged in [&mut self.dat, &mut self.meta] {
            drop(staged.file.take());
            let _ = std::fs::remove_file(&staged.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use fauna_core::data::ContentHash;

    use super::*;
    use crate::backend::StoreBackend;
    use crate::types::{InsertOutcome, ItemRef, JournalOp, JournalRow, WriterId};

    fn row(scope: &str, seq: u64, story: u8) -> JournalRow {
        JournalRow {
            writer: WriterId::NEST_SEQUENCER,
            seq,
            scope: scope.to_string(),
            op: JournalOp::RecordAdded,
            item: ItemRef::Cid(ContentHash::from_digest_dag_cbor([story; 32])),
        }
    }

    /// Lay down a genesis store holding `rows`, then drop the additive
    /// `relay_rows.feed_seq` column — a long-lived store the reconcile must
    /// grow on its next open, and the only way to make an open mutate the
    /// schema at all on an otherwise-current store.
    async fn seed_store_missing_a_column(dir: &Path, rows: &[(&str, u64, u8)]) {
        {
            let backend = SqliteBackend::open(dir).unwrap();
            for (scope, seq, story) in rows {
                backend
                    .insert_row(&row(scope, *seq, *story), None)
                    .await
                    .unwrap();
            }
        }
        let conn = Connection::open(dir.join(ACCOUNT_STORE_DB_FILENAME)).unwrap();
        conn.execute_batch("ALTER TABLE relay_rows DROP COLUMN feed_seq")
            .unwrap();
        assert!(!has_feed_seq(dir), "the seed lacks the column");
    }

    /// Does the store's `relay_rows` carry `feed_seq` — has the reconcile run?
    fn has_feed_seq(dir: &Path) -> bool {
        let conn = Connection::open(dir.join(ACCOUNT_STORE_DB_FILENAME)).unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('relay_rows') WHERE name = 'feed_seq'",
            [],
            |r| r.get::<_, i64>(0).map(|n| n > 0),
        )
        .unwrap()
    }

    /// **A shredded generation's relay residue is found by index.** A form-v2
    /// row is stamped with the generation its cleartext header names, a v1 row
    /// is not; the stamp survives payload eviction; and the sweep drops exactly
    /// the named generation's rows, whoever wrote them.
    #[tokio::test]
    async fn relay_rows_are_swept_by_the_generation_their_header_names() {
        use crate::types::RelayRow;
        const SCOPE: &str = "state-fleet";
        let (g, other) = ([0x61u8; 32], [0x62u8; 32]);
        let relay = |writer: u8, item: u8, entry: Vec<u8>| RelayRow {
            scope: SCOPE.into(),
            item_class: "state-entry".into(),
            writer: WriterId([writer; 32]),
            writer_seq: 1,
            item_key: vec![item; 32],
            op: "state-put".into(),
            entry: Some(entry),
            feed_seq: None,
        };
        // Form v2: the form byte, then the 32-byte generation id in clear.
        let v2 = |generation: &[u8; 32]| [&[2u8][..], generation, &[0xEE; 40]].concat();
        let rows = [
            relay(1, 1, v2(&g)),
            relay(2, 2, v2(&g)),
            relay(1, 3, v2(&other)),
            relay(1, 4, [&[1u8][..], &[0x61; 64]].concat()),
        ];

        let dir = tempfile::tempdir().unwrap();
        let backend = SqliteBackend::open(dir.path()).unwrap();
        for r in &rows {
            backend.relay_put(r).await.unwrap();
        }
        let mut held = backend.relay_generations(SCOPE).await.unwrap();
        held.sort_unstable();
        assert_eq!(
            held,
            vec![g, other],
            "the put stamps the header's generation"
        );

        // Eviction clears the payload, never the stamp.
        backend
            .relay_evict_payload(SCOPE, "state-entry", u64::MAX, &[])
            .await
            .unwrap();
        assert_eq!(
            backend.relay_forget_sealed_under(SCOPE, &g).await.unwrap(),
            2,
            "both writers' rows under the generation go"
        );
        let mut left: Vec<Vec<u8>> = backend
            .relay_rows(SCOPE, "state-entry", &[], u32::MAX)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.item_key)
            .collect();
        left.sort_unstable();
        assert_eq!(left, vec![vec![3u8; 32], vec![4u8; 32]]);
        assert_eq!(backend.relay_generations(SCOPE).await.unwrap(), vec![other]);
    }

    /// The W5.2 notification floor's exact contract (charter § Multi-instance
    /// concurrency, T9): `data_version` moves **iff another connection
    /// commits** — this backend's own writes never move its own reading, and
    /// a sibling connection's committed write always does.
    #[tokio::test]
    async fn data_version_moves_iff_another_connection_commits() {
        let dir = tempfile::tempdir().unwrap();
        let backend = SqliteBackend::open(dir.path()).unwrap();
        let v0 = backend
            .data_version()
            .await
            .unwrap()
            .expect("sqlite has a counter");

        // Own write: the counter must NOT move (a runtime's own pump would
        // otherwise notify itself forever).
        backend.meta_put("w52", b"own-write").await.unwrap();
        let v1 = backend.data_version().await.unwrap().unwrap();
        assert_eq!(v0, v1, "an own-connection write must not move data_version");

        // Another connection commits: the counter must move.
        let sibling = SqliteBackend::open(dir.path()).unwrap();
        sibling.meta_put("w52", b"sibling-write").await.unwrap();
        let v2 = backend.data_version().await.unwrap().unwrap();
        assert_ne!(
            v1, v2,
            "a sibling connection's commit must move data_version"
        );
    }

    /// The journal log is per (scope, writer): the reserved nest-sequencer
    /// name starts every content scope at seq 1, so a second scope's seq 1
    /// is admitted, not refused as equivocation.
    #[tokio::test]
    async fn the_journal_key_is_per_scope() {
        let backend = SqliteBackend::open_in_memory().unwrap();
        for scope in ["content:mail:aa", "content:post:aa"] {
            assert_eq!(
                backend
                    .insert_row(&row(scope, 1, 0xA1), None)
                    .await
                    .unwrap(),
                InsertOutcome::Inserted,
                "{scope}"
            );
        }
    }

    /// **W5.3 — the migration critical section is exclusive** (charter
    /// § Multi-instance concurrency: "store-level advisory locks for the two
    /// genuinely exclusive critical sections — schema migration/adoption, and
    /// the engine-singleton role").
    ///
    /// Several same-account processes may cold-open one store dir at the same
    /// moment. `open` probes the store's columns and *then* `ALTER`s in what
    /// the genesis declares and the store lacks, and a probe-then-rewrite pair
    /// is not a transaction; the WAL conversion in front of it needs a brief
    /// exclusive lock that `busy_timeout` does not cover. The lock is what
    /// makes the second opener wait, find the work done, and skip it.
    ///
    /// **The observable is every opener succeeding:** an interleave inside
    /// the section fails an opener outright — on the second `ADD COLUMN`
    /// (`duplicate column name`) or on `set WAL: database is locked`.
    ///
    /// Latency-independent (convention 14): the threads are released by a
    /// `Barrier` and joined, and *every* assertion is on state after the last
    /// join. Nothing waits on a clock, and a slow machine only serializes the
    /// openers further — which is the outcome asserted.
    ///
    /// **The red direction is probabilistic** (measured 2026-08-14 against
    /// the retired journal rebuild: with the lock removed, 10 of 12 runs
    /// failed). One green run of a broken build proves nothing, so re-verify
    /// over a loop, never a single run.
    #[tokio::test]
    async fn concurrent_cold_opens_reconcile_the_store_once() {
        use std::sync::Barrier;

        const OPENERS: usize = 4;

        let dir = tempfile::tempdir().unwrap();
        seed_store_missing_a_column(
            dir.path(),
            &[("content:mail:aa", 1, 0xA1), ("content:mail:aa", 2, 0xA2)],
        )
        .await;

        let start = Arc::new(Barrier::new(OPENERS));
        let openers: Vec<_> = (0..OPENERS)
            .map(|_| {
                let start = Arc::clone(&start);
                let path = dir.path().to_path_buf();
                std::thread::spawn(move || {
                    start.wait();
                    SqliteBackend::open(&path).map(|_| ())
                })
            })
            .collect();
        for (i, opener) in openers.into_iter().enumerate() {
            opener
                .join()
                .expect("no opener panics")
                .unwrap_or_else(|e| panic!("cold opener {i} failed: {e:#}"));
        }
        assert!(has_feed_seq(dir.path()), "the store ends reconciled");

        // Lossless: both seeded rows survive.
        let backend = SqliteBackend::open(dir.path()).unwrap();
        let rows = backend
            .rows_for_scope("content:mail:aa", &WriterId::NEST_SEQUENCER, 0, 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 2, "the racing openers lost no rows");
    }

    /// **A guarded local write waits for a sibling's commit; it is never
    /// refused by it** (charter § Multi-instance concurrency → *Store
    /// contract*: every non-holder instance is a plain reader/writer of the
    /// store).
    ///
    /// The guarded write reads the writer identity and then inserts. Opened
    /// as a *deferred* transaction that read makes it a reader, and SQLite
    /// does not run the busy handler for a reader asking for the write lock
    /// (it could be waiting on a writer that waits on it): while another
    /// connection holds the lock the insert answers `database is locked` at
    /// once, whatever `busy_timeout` says. Taking the write lock at `BEGIN`
    /// is the one place the wait can happen.
    ///
    /// Latency-independent (convention 14): the holder keeps the write lock
    /// until this connection's busy handler has run — the put is then
    /// provably waiting behind it — or until the put has returned, which is
    /// the refusal. No clock decides either outcome.
    #[tokio::test]
    async fn a_guarded_local_write_waits_behind_a_siblings_write_lock() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::mpsc;

        static WAITED: AtomicBool = AtomicBool::new(false);
        fn note_the_wait(_attempts: i32) -> bool {
            WAITED.store(true, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(1));
            true
        }

        let dir = tempfile::tempdir().unwrap();
        let backend = SqliteBackend::open(dir.path()).unwrap();
        backend.conn.busy_handler(Some(note_the_wait)).unwrap();

        let returned = Arc::new(AtomicBool::new(false));
        let (held_tx, held_rx) = mpsc::channel();
        let holder = {
            let returned = Arc::clone(&returned);
            let path = dir.path().join(ACCOUNT_STORE_DB_FILENAME);
            std::thread::spawn(move || {
                let sibling = Connection::open(path).unwrap();
                sibling
                    .execute_batch(
                        "BEGIN IMMEDIATE;
                         INSERT INTO store_meta (key, value) VALUES ('sibling-probe', x'01');",
                    )
                    .unwrap();
                held_tx.send(()).unwrap();
                while !WAITED.load(Ordering::SeqCst) && !returned.load(Ordering::SeqCst) {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                sibling.execute_batch("COMMIT").unwrap();
            })
        };
        held_rx.recv().unwrap();

        let writer = WriterId([0x57; 32]);
        let entry = StateEntry {
            kind: "kind".into(),
            key: "key".into(),
            scope: "state-fleet".into(),
            value: vec![1],
            merge_meta: None,
            entry_version: 1,
            tombstone: false,
        };
        let journal = JournalRow {
            writer,
            seq: 1,
            scope: entry.scope.clone(),
            op: JournalOp::StatePut,
            item: ItemRef::StateKey {
                kind: entry.kind.clone(),
                key: entry.key.clone(),
                entry_version: 1,
            },
        };
        let outcome = backend
            .state_put_with_row(&entry, &journal, Some(&writer))
            .await;
        returned.store(true, Ordering::SeqCst);
        holder.join().expect("the holder commits");

        assert_eq!(
            outcome.unwrap_or_else(|e| panic!("the write was refused, not queued: {e:#}")),
            InsertOutcome::Inserted
        );
        assert!(
            WAITED.load(Ordering::SeqCst),
            "the write waited for the sibling's commit"
        );
    }

    /// A store dir left by an opener that **died mid-migration** is completed
    /// by the next opener, with no lock-file reconciliation of any kind — the
    /// crash-safety half of the section (`MigrationLock` releases at the
    /// kernel when the holder's last descriptor dies).
    ///
    /// A killed process cannot be staged in-process, so the state it leaves
    /// is staged instead: a store still missing a column plus a
    /// `migration.lock` file that exists and is unlocked — exactly what the
    /// kernel leaves behind. The next open must reconcile normally rather
    /// than refusing or waiting.
    #[tokio::test]
    async fn a_store_left_by_a_dead_migrator_is_completed_by_the_next_opener() {
        let dir = tempfile::tempdir().unwrap();
        seed_store_missing_a_column(dir.path(), &[("content:mail:aa", 1, 0xA1)]).await;
        // The dead holder's residue: the lock file it minted, never unlinked.
        std::fs::write(crate::store::migration_lock_path(dir.path()), b"").unwrap();

        let backend = SqliteBackend::open(dir.path()).unwrap();

        assert!(
            has_feed_seq(dir.path()),
            "the next opener ran the reconcile"
        );
        let rows = backend
            .rows_for_scope("content:mail:aa", &WriterId::NEST_SEQUENCER, 0, 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "the abandoned store's row survived");
    }

    /// **§ 2.2's boot-check law: the verdict runs BEFORE migrations mutate**
    /// (`version-compatibility.md` § 2.2 — the nest's
    /// `check_schema_compatibility`-in-`CacheDb::open` placement). Today's
    /// physical migrations happen to no-op on a future-format store; the next
    /// one need not — so the backend refuses a newer-breaking pair before the
    /// WAL conversion or `migrate()` touch anything. The canary is structural:
    /// the seeded DB carries ONLY `store_meta`, so any table `migrate()`
    /// would have created proves the mutation ran.
    #[test]
    fn a_newer_breaking_store_is_refused_before_migrations_touch_it() {
        let dir = tempfile::tempdir().unwrap();
        {
            let conn = Connection::open(dir.path().join(ACCOUNT_STORE_DB_FILENAME)).unwrap();
            conn.execute_batch(
                "CREATE TABLE store_meta (key TEXT PRIMARY KEY, value BLOB NOT NULL);",
            )
            .unwrap();
            // Bound as byte slices — the encoding every shipped binary writes.
            for key in ["format_version", "min_reader_format_version"] {
                conn.execute(
                    "INSERT INTO store_meta (key, value) VALUES (?1, ?2)",
                    params![key, &b"9"[..]],
                )
                .unwrap();
            }
        }
        let err = match SqliteBackend::open(dir.path()) {
            Ok(_) => panic!("a newer-breaking store must refuse to open"),
            Err(err) => err,
        };
        assert!(
            err.downcast_ref::<crate::store::StoreIncompatible>()
                .is_some(),
            "expected the typed StoreIncompatible refusal, got: {err}"
        );
        let conn = Connection::open(dir.path().join(ACCOUNT_STORE_DB_FILENAME)).unwrap();
        let journal_exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='journal')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            !journal_exists,
            "migrations ran against a store this binary must refuse to touch"
        );
    }

    /// **W5.3 — a meta pair is never observable half-written.** The at-rest
    /// format pair is exactly such a pair, and
    /// [`AccountStore::open`](crate::store::AccountStore::open) *refuses to
    /// guess* at half of one — so a fresh store written as two separate
    /// transactions gives a concurrent cold opener a window in which a
    /// perfectly good store reads as corrupt, and it bails.
    ///
    /// The window is where the test looks: [`pair_window`] fires **between**
    /// the two writes, and the observer is a second connection — the same
    /// vantage the racing opener has. Inside one transaction it must see
    /// *neither* key; regressed to two `meta_put` calls it sees the first
    /// alone, which is the corruption itself. The assertion is on what that
    /// observer recorded, so nothing here depends on timing.
    #[tokio::test]
    async fn a_concurrent_reader_never_sees_half_a_meta_pair() {
        let dir = tempfile::tempdir().unwrap();
        let backend = SqliteBackend::open(dir.path()).unwrap();
        // `Connection` is Send but not Sync, and the hook is `Fn + Send +
        // Sync`; the mutex is only what carries it across that bound.
        let observer =
            Mutex::new(Connection::open(dir.path().join(ACCOUNT_STORE_DB_FILENAME)).unwrap());

        let seen: Arc<Mutex<Option<(bool, bool)>>> = Arc::new(Mutex::new(None));
        let recorder = Arc::clone(&seen);
        let _installed = pair_window::install(Arc::new(move || {
            let observer = observer.lock().unwrap();
            let held = |key: &str| {
                observer
                    .query_row(
                        "SELECT 1 FROM store_meta WHERE key = ?1",
                        params![key],
                        |_| Ok(()),
                    )
                    .optional()
                    .unwrap()
                    .is_some()
            };
            *recorder.lock().unwrap() = Some((held("pair.a"), held("pair.b")));
        }));

        backend
            .meta_put_pair_max(("pair.a", 1), ("pair.b", 2))
            .await
            .unwrap();

        assert_eq!(
            seen.lock().unwrap().expect("the window fired"),
            (false, false),
            "a second connection saw a half-written pair mid-transaction — the exact \
             state AccountStore::open reads as corrupt and refuses"
        );
        // And the pair is whole once committed.
        assert_eq!(
            backend.meta_get("pair.a").await.unwrap().as_deref(),
            Some(&b"1"[..])
        );
        assert_eq!(
            backend.meta_get("pair.b").await.unwrap().as_deref(),
            Some(&b"2"[..])
        );
    }

    /// **A multi-key read never assembles a picture from two instants** — the
    /// read-side mirror of the pair write, and the serialization half of the
    /// re-author loss.
    ///
    /// The succession fence writes the writer stamp and the pending-re-author
    /// marker in ONE transaction, and the re-author pass filters the marker
    /// against the stamp — so a pass that reads them in two transactions can
    /// see a **pre-fence stamp beside a post-fence marker**, drop the marker's
    /// own predecessor as if it were the store's own writer, and clear a marker
    /// that names a real un-pushed tail (charter § The store device principal →
    /// succession decision 3; `principles.md` No user-data loss).
    ///
    /// The window is where the property lives: [`pair_window`] fires **between**
    /// the reads and a second connection lands the whole fence there. A caller
    /// -side assertion could not tell this apart from a loop of `meta_get`s,
    /// which is exactly the regression it must catch — so the assertion is that
    /// the returned triple is wholly pre-fence or wholly post-fence, never
    /// mixed.
    #[tokio::test]
    async fn a_multi_key_read_never_straddles_a_concurrent_commit() {
        let dir = tempfile::tempdir().unwrap();
        let backend = SqliteBackend::open(dir.path()).unwrap();
        backend.meta_put("snap.stamp", b"before").await.unwrap();

        // `Connection` is Send but not Sync and the hook is `Fn + Send + Sync`;
        // the mutex is only what carries the writer across that bound.
        let writer =
            Mutex::new(Connection::open(dir.path().join(ACCOUNT_STORE_DB_FILENAME)).unwrap());
        let fired = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = Arc::clone(&fired);
        let _installed = pair_window::install(Arc::new(move || {
            // Fire the fence ONCE, in the first gap — the pass's own race.
            if count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) != 0 {
                return;
            }
            let writer = writer.lock().unwrap();
            writer
                .execute(
                    "INSERT INTO store_meta (key, value) VALUES (?1, ?2)
                     ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                    params!["snap.stamp", &b"after"[..]],
                )
                .unwrap();
            writer
                .execute(
                    "INSERT INTO store_meta (key, value) VALUES (?1, ?2)",
                    params!["snap.marker", &b"owed"[..]],
                )
                .unwrap();
        }));

        let got = backend
            .meta_get_all(&["snap.stamp", "snap.marker"])
            .await
            .unwrap();
        drop(_installed);

        assert!(
            fired.load(std::sync::atomic::Ordering::SeqCst) > 0,
            "the window never fired — the probe staged no concurrent commit"
        );
        let picture = (got[0].as_deref(), got[1].as_deref());
        assert!(
            picture == (Some(&b"before"[..]), None)
                || picture == (Some(&b"after"[..]), Some(&b"owed"[..])),
            "the read straddled a concurrent commit and returned a picture that never \
             existed: {picture:?}. Assembled from two instants, the re-author pass \
             filters a post-fence marker against a pre-fence stamp and clears the tail \
             it was meant to protect."
        );
    }

    /// **The open reads the format pair as of one instant** — the read-side
    /// half of W5.3's pair, which the one-transaction write alone does not
    /// give: a pair that is never *written* half can still be *read* half, by
    /// two separate reads with a sibling's whole stamp landing between them.
    /// `format_version` is then absent and `min_reader_format_version`
    /// present, and the open refuses a good store as corrupt — the red a
    /// concurrent cold assembly showed under contention ("half a version pair
    /// (format_version None, min_reader_format_version Some(1))").
    ///
    /// [`pair_window`] fires between the multi-key read's keys, and a second
    /// connection — the sibling's cold open — stamps the whole pair there, in
    /// the FIRST window the open passes through. Read in one multi-key read,
    /// that window is the read's and the sibling's stamp lands (WAL: a reader
    /// never blocks a writer). Read as two `meta_get`s, no window opens until
    /// the open's own stamp is mid-write, where the sibling is locked out —
    /// so "the sibling's stamp landed" is the claim that the pair was read as
    /// of one instant, which no caller-side assertion could make.
    #[tokio::test]
    async fn the_open_reads_the_format_pair_as_of_one_instant() {
        use crate::store::{
            FORMAT_VERSION, META_FORMAT_VERSION, META_MIN_READER, MIN_READER_FORMAT_VERSION,
            verify_and_stamp_format,
        };
        let dir = tempfile::tempdir().unwrap();
        let backend = SqliteBackend::open(dir.path()).unwrap();

        let sibling = Connection::open(dir.path().join(ACCOUNT_STORE_DB_FILENAME)).unwrap();
        // Locked out is an answer here, not something to wait on.
        sibling.busy_timeout(std::time::Duration::ZERO).unwrap();
        let sibling = Mutex::new(sibling);
        let landed: Arc<Mutex<Option<bool>>> = Arc::new(Mutex::new(None));
        let recorder = Arc::clone(&landed);
        let _installed = pair_window::install(Arc::new(move || {
            // The sibling's stamp is tried ONCE, in the first window.
            let mut landed = recorder.lock().unwrap();
            if landed.is_some() {
                return;
            }
            let sibling = sibling.lock().unwrap();
            let stamp = || -> rusqlite::Result<()> {
                let tx = sibling.unchecked_transaction()?;
                for (key, value) in [
                    (META_FORMAT_VERSION, FORMAT_VERSION),
                    (META_MIN_READER, MIN_READER_FORMAT_VERSION),
                ] {
                    tx.execute(
                        "INSERT INTO store_meta (key, value) VALUES (?1, ?2)",
                        params![key, value.to_string().as_bytes()],
                    )?;
                }
                tx.commit()
            };
            *landed = Some(stamp().is_ok());
        }));

        let opened =
            verify_and_stamp_format(&backend, FORMAT_VERSION, MIN_READER_FORMAT_VERSION).await;
        drop(_installed);

        assert_eq!(
            *landed.lock().unwrap(),
            Some(true),
            "the sibling's stamp did not land inside the open's read of the format pair \
             (None: no window fired; Some(false): the first window was the open's own \
             stamp, mid-write) — the pair is read as separate gets, and a sibling's stamp \
             between them shows this open half a pair"
        );
        opened.expect(
            "a sibling's whole stamp landing mid-read must not read as half a version pair",
        );
        for key in [META_FORMAT_VERSION, META_MIN_READER] {
            assert!(
                backend.meta_get(key).await.unwrap().is_some(),
                "{key} is stamped after both opens"
            );
        }
    }

    /// **The pair write is rising-only, per key** — the race half of the finding:
    /// the fresh stamp races a version-skewed sibling's cold open, and an
    /// unconditional upsert lets whoever runs LAST win, so an older binary
    /// could pull the pair down. Per-key max makes the pair a lattice join —
    /// any order converges to the honest union. The 10-vs-2 leg pins the
    /// numeric compare: values rest as ASCII-decimal blobs, and a bytewise
    /// compare would order "10" below "2".
    #[tokio::test]
    async fn a_racing_lower_stamp_cannot_pull_the_version_pair_down() {
        let b = SqliteBackend::open_in_memory().unwrap();
        b.meta_put_pair_max(("race.v", 2), ("race.min", 2))
            .await
            .unwrap();
        b.meta_put_pair_max(("race.v", 1), ("race.min", 1))
            .await
            .unwrap();
        assert_eq!(b.meta_get("race.v").await.unwrap().unwrap(), b"2");
        assert_eq!(
            b.meta_get("race.min").await.unwrap().unwrap(),
            b"2",
            "a lower write landed second and pulled the floor down"
        );
        // Per-key independence: a mixed stamp rises only where it is higher.
        b.meta_put_pair_max(("race.v", 3), ("race.min", 1))
            .await
            .unwrap();
        assert_eq!(b.meta_get("race.v").await.unwrap().unwrap(), b"3");
        assert_eq!(b.meta_get("race.min").await.unwrap().unwrap(), b"2");
        // Numeric, not bytewise: "10" must beat "2".
        b.meta_put_pair_max(("race.v", 10), ("race.min", 10))
            .await
            .unwrap();
        b.meta_put_pair_max(("race.v", 2), ("race.min", 2))
            .await
            .unwrap();
        assert_eq!(b.meta_get("race.v").await.unwrap().unwrap(), b"10");
        assert_eq!(b.meta_get("race.min").await.unwrap().unwrap(), b"10");
    }

    /// A store dir is shared by every same-account process, so a sibling can
    /// open it while this one is mid-transfer. That open's staging sweep must
    /// leave the live slot alone — its files are locked — and the transfer
    /// then adopts as if no one had opened. (The crash half, a dead holder's
    /// files swept, is the conformance case
    /// `a_transfer_interrupted_before_adoption_reopens_clean`.) Mutate: drop
    /// the `try_lock` skip in `sweep_staged_segments` and the sibling's open
    /// deletes the files under the live slot.
    #[tokio::test]
    async fn a_siblings_open_never_sweeps_a_live_transfer() {
        use crate::conformance::fixtures::{SEG_ACTOR, real_segment};

        let dir = tempfile::tempdir().unwrap();
        let backend = SqliteBackend::open(dir.path()).unwrap();
        let (dat, meta) = real_segment("post", 1, SEG_ACTOR, &[b"one record"]);
        let mut staged = backend.segment_stage().await.unwrap();
        staged.write(SegmentHalf::Dat, &dat).await.unwrap();
        staged.write(SegmentHalf::Meta, &meta).await.unwrap();

        let _sibling = SqliteBackend::open(dir.path()).unwrap();
        let staging: Vec<_> = std::fs::read_dir(dir.path().join(SEGMENT_DIR_NAME))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(STAGING_PREFIX))
            .collect();
        assert_eq!(staging.len(), 2, "the live slot survived: {staging:?}");

        let staged_meta = staged.meta().await.unwrap();
        let admission = crate::segments::admit(
            staged.dat_reader().await.unwrap(),
            &staged_meta,
            &SEG_ACTOR,
            None,
        )
        .unwrap();
        let key = SegmentKey {
            scope: "content:post:ab".into(),
            kind: "post".into(),
            segment_id: 1,
        };
        assert!(
            backend
                .segment_adopt(&key, staged, &admission.blocks)
                .await
                .unwrap(),
            "and it adopts"
        );
    }
}
