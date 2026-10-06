//! SQLite-backed key-value storage for MLS state.
//!
//! The store carries the two-number at-rest version scheme of
//! `version-compatibility.md` § 2.2 (see [`crate::version`]): every open checks
//! the database's stamp *before* touching it, reconciles any additive column the
//! current DDL has grown, and restamps — never lowering a newer build's numbers.
//! `mls.db` is user-irrecoverable, so an unreadable database is refused honestly
//! and left byte-for-byte intact (I1), never migrated or rewritten on a guess.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

use anyhow::{Context, Result};
use rusqlite::Connection;
use thiserror::Error;

use crate::version::{
    CURRENT_SCHEMA_VERSION, MIN_READER_SCHEMA_VERSION, SchemaIncompatible, SchemaVerdict,
    check_schema_compatibility,
};

/// The `mls_state` `store_type` of the engine's last-seen replica listing
/// ([`SqliteStorage::list_replica_listed`]); one row per channel id.
const REPLICA_LISTED: &str = "_replica_listed";

/// One row of the `mls_state` table: `(store_type, key, value)`.
pub type MlsStateRow = (String, Vec<u8>, Vec<u8>);

/// Returned by [`SqliteStorage::open`] when another live conversations engine
/// — usually another process, possibly another engine in this one — holds the
/// **conversations-engine role lock** over this database.
///
/// Like [`SchemaIncompatible`], it is **not** a generic open failure: callers
/// downcast it (`err.downcast_ref::<StateServedElsewhere>()`) to refuse
/// honestly — "your conversations are served in another instance" — rather
/// than report a broken store. The state is intact and untouched; the holder
/// is serving it.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error(
    "the MLS state at {path} is served by another instance (its role lock is held) \
     — refusing to open a second conversations engine over it; the state is \
     intact, and the holding instance is serving it"
)]
pub struct StateServedElsewhere {
    pub path: String,
}

/// Returned by every statement on a storage whose conversations-engine role has
/// been **handed over** — [`SqliteStorage::retire`] released the role lock so a
/// successor engine over the same `mls_state.db` could take it.
///
/// A retired engine's object graph routinely outlives the hand-over (a shell
/// field nobody nils, a stashed `Arc`, a rider that has not noticed the close
/// yet), and `mls_state.db` is class-5, user-irrecoverable state: two engines
/// advancing one group's epochs fork the ratchet no matter how transactional
/// each individual write is (`account-data-plane.md` § Multi-instance
/// concurrency). So a retired storage does **not** degrade quietly into a second
/// writer — it refuses, typed and loud, the same posture as the deliberately
/// un-WAL'd `busy_timeout` 0 tripwire beside it.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error(
    "the MLS state at {path} was retired — its conversations-engine role was \
     handed to a newer engine over the same database, so this handle must not \
     read or write it; its holder should have been dropped"
)]
pub struct MlsStateRetired {
    pub path: String,
}

/// The conversations-engine role lock's path: the guarded database's own path
/// with `.lock` appended (`mls_state.db` → `mls_state.db.lock`).
///
/// **Derived from the file, not a fixed sibling name** (build refinement
/// 2026-08-15 to the ruled `mls.lock` shape — recorded in
/// `account-data-plane.md` § Multi-instance concurrency): a fixed name would
/// key the guard to the *directory*, so any layout with two MLS databases in
/// one directory — a `NamedTempFile` test's shared OS temp dir above all —
/// would contend on one machine-global lock. Appending to the guarded file's
/// own name keeps the ruling's beside-the-state placement while keying the
/// guard to exactly the file it covers.
pub fn role_lock_path(db_path: &Path) -> PathBuf {
    let mut os = db_path.as_os_str().to_os_string();
    os.push(".lock");
    PathBuf::from(os)
}

/// The live connection, held for one statement. A newtype over the guard rather
/// than the guard itself because the connection is now an `Option` (see
/// [`SqliteStorage::conn`]): this keeps all 22 statement sites reading as
/// `conn.execute(…)` instead of each unwrapping the same invariant, and keeps
/// that unwrap in ONE place that [`SqliteStorage::conn`] has already proved.
struct ConnGuard<'a>(MutexGuard<'a, Option<Connection>>);

impl std::ops::Deref for ConnGuard<'_> {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        self.0.as_ref().expect(
            "SqliteStorage::conn refuses a retired (closed) handle before handing out a guard",
        )
    }
}

/// A simple key-value store backed by SQLite for persisting MLS state.
///
/// Keys and values are arbitrary byte slices, partitioned by a string
/// `store_type` (e.g. "key_package", "group_state"). The underlying
/// table uses `(store_type, key)` as a composite primary key.
#[derive(Debug)]
pub struct SqliteStorage {
    /// `None` once [`Self::retire`] has closed it. Behind an `Option` for the
    /// same reason `role_lock` is, and it is the same hand-over: on Windows an
    /// open handle is not merely untidy, it makes the file **undeletable**
    /// (`ERROR_SHARING_VIOLATION`), so a retired-but-still-open connection kept
    /// a signed-out user's `mls.db` on disk through the erase that was supposed
    /// to remove it. POSIX `unlink` tolerates open
    /// handles, which is why only Windows ever showed it. Releasing here costs
    /// nothing: every statement already refuses after a retire, so there is no
    /// reachable state in which this connection could still be used.
    conn: Mutex<Option<Connection>>,
    /// The held conversations-engine role lock (`None` for in-memory stores).
    /// Held — and thereby locked — for this storage's lifetime, which is the
    /// engine's lifetime; RAII, the kernel releases on drop or process death.
    ///
    /// Behind a `Mutex<Option<_>>` rather than a plain field because the role is
    /// also handed over **explicitly**, ahead of this storage's drop:
    /// [`Self::retire`] takes the `File` out and drops it, releasing the lock at
    /// a point the code chooses rather than whenever the last `Arc` anywhere in
    /// the process — including inside a foreign shell's object graph, which no
    /// Rust code can see — happens to go away.
    role_lock: Mutex<Option<File>>,
    /// Set by [`Self::retire`]; checked by [`Self::conn`], which every statement
    /// goes through. See [`MlsStateRetired`] for why a retired handle refuses.
    retired: AtomicBool,
    /// This database's path, for [`MlsStateRetired`]'s message. `:memory:` for
    /// [`Self::open_in_memory`].
    path: String,
}

impl SqliteStorage {
    /// Open (or create) a SQLite database at `path`, verify its at-rest schema
    /// version, bring its tables up to the current DDL, and stamp it.
    ///
    /// **Acquires the conversations-engine role lock first** — a
    /// kernel-arbitrated advisory lock on [`role_lock_path`] (`flock` unix,
    /// `LockFileEx` windows; no probe window, automatic crash release), held
    /// for this storage's lifetime. MLS exclusivity is permanent, not a
    /// transitional shim: two engines advancing one group's epochs fork the
    /// ratchet even when every SQLite write is transactional, so `mls_state.db`
    /// gets a **role lock**, not a store upgrade (`account-data-plane.md`
    /// § Multi-instance concurrency, ruled 2026-08-15). A held lock returns
    /// [`StateServedElsewhere`] (downcast it from the [`anyhow::Error`]);
    /// a lock-file I/O failure **fails closed** — unlike the account instance
    /// lock's degrade-open posture, because this state is class 5
    /// (user-irrecoverable): a directory that cannot host the zero-byte lock
    /// file cannot host the SQLite journal either, so degrading open would
    /// trade a near-impossible availability corner for a silent ratchet fork.
    ///
    /// Returns [`SchemaIncompatible`] (downcast it from the [`anyhow::Error`])
    /// when the database was written by a build whose breaking changes this one
    /// predates. In that case **nothing is written** — the user's MLS state is
    /// left exactly as found, and the app should tell them to update rather than
    /// present an empty or half-read conversation list.
    pub fn open(path: &Path) -> Result<Self> {
        let role_lock = Self::acquire_role_lock(path)?;
        let conn = Connection::open(path)?;
        // The ratified tripwire's second half, explicit rather than assumed
        // (the W5.6 (account-data-plane.md § Workstreams) build found the driver defaults this to 5 s, so "neither
        // pragma is set" did not mean 0): with the role lock making
        // cross-process access unreachable and this connection behind a
        // `Mutex`, no legitimate writer can ever contend — a `SQLITE_BUSY`
        // here is a guard regression, and it must fail HARD and NOW, not
        // after a silent five-second stall. See `prepare`'s doc comment.
        conn.busy_timeout(std::time::Duration::ZERO)
            .context("pinning mls state busy_timeout to 0 (the ratified tripwire)")?;
        Self::prepare(&conn)?;
        Ok(Self {
            conn: Mutex::new(Some(conn)),
            role_lock: Mutex::new(Some(role_lock)),
            retired: AtomicBool::new(false),
            path: path.display().to_string(),
        })
    }

    /// Hand the conversations-engine role over: release the role lock and refuse
    /// every later statement on this handle ([`MlsStateRetired`]).
    ///
    /// **Why an explicit hand-over exists at all.** The role lock is held for the
    /// engine's lifetime (`account-data-plane.md` § Multi-instance concurrency),
    /// and RAII makes that exactly right for the *process* case it was ruled for.
    /// It is not enough for the in-process case: one app instance rebuilds its
    /// conversations session for the same account on every re-login, account
    /// switch and factory-reset re-onboard, and the new engine is constructed
    /// **before** anything releases the old one — so the successor asks for a
    /// lock its own predecessor still holds and is refused
    /// `StateServedElsewhere`, a message about *another instance* that is a lie
    /// here. Waiting for the predecessor's last `Arc` to drop is not a fix: those
    /// references live in three languages (a Swift `ConversationsVM.session`, a
    /// C# host field, a stashed FFI `Arc`), and correctness that depends on
    /// counting them across an FFI boundary is exactly the kind that rots.
    ///
    /// This is **not** a relaxation of the ruled exclusivity: after a retire
    /// there is still exactly one handle that may touch the database — the
    /// successor — and the predecessor is provably not it, because it refuses.
    /// Cross-process contention is untouched: another instance's engine is not
    /// reachable from here, so it keeps the lock and this process keeps being
    /// refused, which is the case the honest refusal was written for.
    ///
    /// Flushes the provider snapshot first (via the engine, see
    /// [`crate::engine::MlsEngine::retire`]) — this method itself only releases.
    /// Idempotent.
    pub fn retire(&self) {
        // FIRST: every statement refuses from here on, which is what makes
        // taking the connection out below unobservable to any caller.
        self.retired.store(true, Ordering::SeqCst);
        // Dropping the `File` releases the advisory lock; the kernel does the
        // rest. Taking it out of the `Option` also makes the release idempotent.
        drop(self.role_lock.lock().expect("lock poisoned").take());
        // And close the database itself. The role lock alone is not the whole
        // hand-over on Windows: an open SQLite connection keeps an OS handle on
        // `mls.db`, and Windows refuses to delete an open file
        // (`ERROR_SHARING_VIOLATION`, `os error 32`) — so a sign-out's erase
        // aborted on the very first actor scope and left the signed-out user's
        // conversations readable on disk. POSIX
        // `unlink` unlinks an open file happily, which is precisely why this was
        // invisible on linux/tui/apple for as long as it existed.
        //
        // Refcount-independent by construction, like the rest of this hand-over:
        // the handle goes when the code says so, not when the last `Arc` — some
        // of which live in a foreign shell's object graph that no Rust code can
        // see — happens to drop.
        drop(self.conn.lock().expect("lock poisoned").take());
    }

    /// The connection, or [`MlsStateRetired`] once the role has been handed over.
    /// **Every** statement in this file goes through here — that is what makes
    /// the refusal total rather than a courtesy check on the paths someone
    /// remembered.
    fn conn(&self) -> Result<ConnGuard<'_>> {
        if self.retired.load(Ordering::SeqCst) {
            return Err(MlsStateRetired {
                path: self.path.clone(),
            }
            .into());
        }
        let guard = self.conn.lock().expect("lock poisoned");
        if guard.is_none() {
            // Belt and braces: `retire` sets `retired` before it takes the
            // connection, so this is unreachable through the check above. It
            // exists so that a future caller who adds a second closer cannot
            // turn the `expect` in `ConnGuard::deref` into a panic.
            return Err(MlsStateRetired {
                path: self.path.clone(),
            }
            .into());
        }
        Ok(ConnGuard(guard))
    }

    /// Create an in-memory SQLite database for transient use (e.g. WASM).
    /// No role lock: an in-memory store is unshareable by construction.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::prepare(&conn)?;
        Ok(Self {
            conn: Mutex::new(Some(conn)),
            role_lock: Mutex::new(None),
            retired: AtomicBool::new(false),
            path: ":memory:".to_string(),
        })
    }

    /// Take the conversations-engine role for the database at `db_path`, or
    /// refuse. Non-blocking: the role is *won*, never queued for — a
    /// non-holder has a full life serving every non-conversations surface,
    /// and the holder is by construction the (app, account)'s one live
    /// engine. The lock file is never truncated and never deleted
    /// (`fauna_core::fs_lock` owns those invariants).
    fn acquire_role_lock(db_path: &Path) -> Result<File> {
        let lock_path = role_lock_path(db_path);
        // Fail closed on I/O — see `open`'s doc comment.
        let file = fauna_core::fs_lock::open_lock_file(&lock_path).with_context(|| {
            format!(
                "opening the conversations-engine role lock at {} (refusing to \
                 run an unguarded MLS engine — this state is user-irrecoverable)",
                lock_path.display()
            )
        })?;
        match file.try_lock() {
            Ok(()) => Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => Err(StateServedElsewhere {
                path: db_path.display().to_string(),
            }
            .into()),
            Err(std::fs::TryLockError::Error(e)) => Err(e).with_context(|| {
                format!(
                    "taking the conversations-engine role lock at {} (refusing to \
                     run an unguarded MLS engine — this state is user-irrecoverable)",
                    lock_path.display()
                )
            }),
        }
    }

    /// The open sequence, in the one order that is I1-safe: **check before
    /// mutating**, then create missing tables, reconcile grown columns, stamp —
    /// the three mutations in **one transaction**, so a database holding this
    /// store's tables without a stamp is unrepresentable for every current
    /// writer (a crash mid-open rolls back to exactly the file it found).
    ///
    /// **An unstamped database that already holds tables is refused**, never
    /// adopted. Only a build predating the schema-version scheme could have
    /// written one, and the pre-scheme adoption — which read such a file as the
    /// baseline and stamped it — was retired by the compat-remnant sweep
    /// (`version-compatibility.md` § Dimension 2, program 4): no `mls.db` from
    /// before the scheme exists anywhere. A missing stamp on an **empty** file is
    /// the fresh-install case and opens as this binary's version.
    ///
    /// **Deliberate tripwire — do not "modernize" (ratified 2026-08-15,
    /// `account-data-plane.md` § Multi-instance concurrency):** this open sets
    /// neither `PRAGMA journal_mode=WAL` nor a `busy_timeout`. The role lock
    /// above is what makes cross-process access safe; leaving the connection
    /// on the rollback journal with a zero busy-timeout means a guard
    /// regression's first symptom is a hard `SQLITE_BUSY` write error — loud
    /// and diagnosable — never silent interleaving of two engines' writes.
    fn prepare(conn: &Connection) -> Result<()> {
        let (db_v, db_min) = match Self::read_schema_meta(conn)? {
            Some(stamp) => stamp,
            None if Self::holds_any_table(conn)? => {
                // Refuse without a single write, exactly like the newer-breaking
                // arm below.
                anyhow::bail!(
                    "mls.db carries no schema_meta stamp but already holds tables — \
                     a pre-scheme database, whose adoption was retired by the \
                     compat-remnant sweep; refusing to open it"
                );
            }
            // A fresh (empty) database: this binary's own version.
            None => (CURRENT_SCHEMA_VERSION, MIN_READER_SCHEMA_VERSION),
        };
        if let SchemaVerdict::Incompatible {
            db_v,
            db_min,
            bin_v,
        } = check_schema_compatibility(db_v, db_min, CURRENT_SCHEMA_VERSION)
        {
            // Refuse without a single write — not even creating `schema_meta`.
            return Err(SchemaIncompatible {
                db_v,
                db_min,
                bin_v,
            }
            .into());
        }
        let tx = conn
            .unchecked_transaction()
            .context("begin the mls.db open transaction")?;
        Self::init_table(&tx)?;
        Self::reconcile_added_columns(&tx)?;
        Self::record_schema_meta(&tx)?;
        tx.commit().context("commit the mls.db open transaction")?;
        Ok(())
    }

    /// Read the recorded `(schema_version, min_reader_version)`, or `None` when
    /// the database carries no stamp at all (no `schema_meta` table, or no row
    /// in it). Uses the shared
    /// [`fauna_core::sqlite_schema_meta::read_schema_meta`] SQL once a row is
    /// known to exist; narrows the shared `u32` back to this store's own `u16`.
    fn read_schema_meta(conn: &Connection) -> Result<Option<(u16, u16)>> {
        let stamped: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master \
                 WHERE type = 'table' AND name = 'schema_meta')",
            [],
            |r| r.get(0),
        )?;
        if !stamped {
            return Ok(None);
        }
        let has_row: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM schema_meta WHERE id = 1)",
            [],
            |r| r.get(0),
        )?;
        if !has_row {
            return Ok(None);
        }
        // The row exists, so the shared read never reaches its absent-row
        // default; the argument is inert here.
        let (v, min) =
            fauna_core::sqlite_schema_meta::read_schema_meta(conn, CURRENT_SCHEMA_VERSION as u32)?;
        Ok(Some((v as u16, min as u16)))
    }

    /// Whether the database holds any table besides an (empty) `schema_meta` —
    /// the line between a fresh file and an unstamped one some build wrote.
    fn holds_any_table(conn: &Connection) -> Result<bool> {
        Ok(fauna_core::sqlite_schema_meta::managed_tables(conn)?
            .iter()
            .any(|t| t != "schema_meta"))
    }

    /// Stamp this binary's `(CURRENT, MIN_READER)` into the single row via the
    /// shared [`fauna_core::sqlite_schema_meta::record_schema_meta`] — see
    /// that fn's doc for the "never restamp down" guard this relies on.
    fn record_schema_meta(conn: &Connection) -> Result<()> {
        let now = fauna_core::data::Timestamp::now_secs_or_zero();
        fauna_core::sqlite_schema_meta::record_schema_meta(
            conn,
            CURRENT_SCHEMA_VERSION as u32,
            MIN_READER_SCHEMA_VERSION as u32,
            now,
        )
        .context("record schema_meta")
    }

    /// Add any column the current DDL declares that this (older) database lacks.
    /// Thin wrapper over the shared
    /// [`fauna_core::sqlite_schema_meta::reconcile_added_columns`] (see that
    /// fn's doc for the mechanism and the outage class it guards
    /// against); the reference schema is built from [`Self::init_table`], so
    /// the DDL stays the single source of truth.
    fn reconcile_added_columns(conn: &Connection) -> Result<()> {
        fauna_core::sqlite_schema_meta::reconcile_added_columns(conn, Self::init_table)
    }

    fn init_table(conn: &Connection) -> Result<()> {
        conn.execute_batch(fauna_core::sqlite_schema_meta::SCHEMA_META_DDL)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS mls_state (
                store_type TEXT NOT NULL,
                key        BLOB NOT NULL,
                value      BLOB NOT NULL,
                PRIMARY KEY (store_type, key)
            );
            CREATE TABLE IF NOT EXISTS send_sequences (
                channel_id BLOB NOT NULL PRIMARY KEY,
                next_seq INTEGER NOT NULL DEFAULT 1
            );
            CREATE TABLE IF NOT EXISTS recv_sequences (
                channel_id BLOB NOT NULL,
                sender_id BLOB NOT NULL,
                last_seq INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (channel_id, sender_id)
            );
            CREATE TABLE IF NOT EXISTS dm_channels (
                peer_actor_id BLOB NOT NULL PRIMARY KEY,
                channel_id BLOB NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS read_timestamps (
                peer_actor_id BLOB NOT NULL PRIMARY KEY,
                last_read_ms INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS group_channels (
                group_id TEXT NOT NULL PRIMARY KEY,
                channel_id BLOB NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS channel_nest_url (
                channel_id BLOB PRIMARY KEY,
                nest_url TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS active_groups (
                channel_id BLOB NOT NULL PRIMARY KEY,
                mls_group_id BLOB NOT NULL
            );",
        )?;
        Ok(())
    }

    /// Insert or replace a value for the given `(store_type, key)` pair.
    pub fn put(&self, store_type: &str, key: &[u8], value: &[u8]) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT OR REPLACE INTO mls_state (store_type, key, value) VALUES (?1, ?2, ?3)",
            rusqlite::params![store_type, key, value],
        )?;
        Ok(())
    }

    /// Retrieve the value for a `(store_type, key)` pair, or `None` if it
    /// does not exist.
    pub fn get(&self, store_type: &str, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let conn = self.conn()?;
        let mut stmt =
            conn.prepare("SELECT value FROM mls_state WHERE store_type = ?1 AND key = ?2")?;
        let mut rows = stmt.query(rusqlite::params![store_type, key])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    /// Delete the entry for a `(store_type, key)` pair. No-op if it does
    /// not exist.
    pub fn delete(&self, store_type: &str, key: &[u8]) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM mls_state WHERE store_type = ?1 AND key = ?2",
            rusqlite::params![store_type, key],
        )?;
        Ok(())
    }

    /// Get and increment the next send sequence number for a channel.
    pub fn next_send_sequence(&self, channel_id: &[u8; 32]) -> Result<u64> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO send_sequences (channel_id, next_seq) VALUES (?1, 1)
             ON CONFLICT(channel_id) DO UPDATE SET next_seq = next_seq + 1",
            rusqlite::params![channel_id.as_slice()],
        )?;
        let seq: i64 = conn.query_row(
            "SELECT next_seq FROM send_sequences WHERE channel_id = ?1",
            rusqlite::params![channel_id.as_slice()],
            |row| row.get(0),
        )?;
        Ok(seq as u64)
    }

    /// Get the last received sequence number from a sender on a channel.
    pub fn last_recv_sequence(&self, channel_id: &[u8; 32], sender_id: &[u8; 32]) -> Result<u64> {
        let conn = self.conn()?;
        let result = conn.query_row(
            "SELECT last_seq FROM recv_sequences WHERE channel_id = ?1 AND sender_id = ?2",
            rusqlite::params![channel_id.as_slice(), sender_id.as_slice()],
            |row| row.get::<_, i64>(0),
        );
        match result {
            Ok(seq) => Ok(seq as u64),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
            Err(e) => Err(e.into()),
        }
    }

    /// Update the last received sequence number from a sender on a channel.
    pub fn set_recv_sequence(
        &self,
        channel_id: &[u8; 32],
        sender_id: &[u8; 32],
        seq: u64,
    ) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO recv_sequences (channel_id, sender_id, last_seq) VALUES (?1, ?2, ?3)
             ON CONFLICT(channel_id, sender_id) DO UPDATE SET last_seq = ?3",
            rusqlite::params![channel_id.as_slice(), sender_id.as_slice(), seq as i64],
        )?;
        Ok(())
    }

    /// Store a blob epoch key for a channel at a given epoch.
    ///
    /// Uses the generic `put` method with store_type `"blob_epoch_key"` and
    /// a composite key of `channel_id ++ epoch.to_be_bytes()`.
    pub fn put_blob_epoch_key(
        &self,
        channel_id: &[u8; 32],
        epoch: u64,
        key: &[u8; 32],
    ) -> Result<()> {
        let mut composite_key = Vec::with_capacity(40);
        composite_key.extend_from_slice(channel_id);
        composite_key.extend_from_slice(&epoch.to_be_bytes());
        self.put("blob_epoch_key", &composite_key, key)
    }

    /// Retrieve the blob epoch key for a channel at a given epoch.
    ///
    /// Returns `Some([u8; 32])` if found and exactly 32 bytes, `None` otherwise.
    pub fn get_blob_epoch_key(
        &self,
        channel_id: &[u8; 32],
        epoch: u64,
    ) -> Result<Option<[u8; 32]>> {
        let mut composite_key = Vec::with_capacity(40);
        composite_key.extend_from_slice(channel_id);
        composite_key.extend_from_slice(&epoch.to_be_bytes());
        match self.get("blob_epoch_key", &composite_key)? {
            Some(v) if v.len() == 32 => {
                let mut key = [0u8; 32];
                key.copy_from_slice(&v);
                Ok(Some(key))
            }
            _ => Ok(None),
        }
    }

    /// Store a group's **room-post** secret at a given epoch — the
    /// [`crate::MlsEngine::export_room_post_secret`] value, kept when the group
    /// advances past `epoch` so a room-restricted post sealed there still
    /// opens for a member who held it (`ui/feed.md` § Encryption at rest →
    /// *Room-restricted — the ruling*, ruling 6). The
    /// [`Self::put_blob_epoch_key`] shape under its own `store_type`.
    pub fn put_room_post_epoch_key(
        &self,
        channel_id: &[u8; 32],
        epoch: u64,
        key: &[u8; 32],
    ) -> Result<()> {
        let mut composite_key = Vec::with_capacity(40);
        composite_key.extend_from_slice(channel_id);
        composite_key.extend_from_slice(&epoch.to_be_bytes());
        self.put("room_post_epoch_key", &composite_key, key)
    }

    /// Retrieve a room-post secret [`Self::put_room_post_epoch_key`] kept.
    pub fn get_room_post_epoch_key(
        &self,
        channel_id: &[u8; 32],
        epoch: u64,
    ) -> Result<Option<[u8; 32]>> {
        let mut composite_key = Vec::with_capacity(40);
        composite_key.extend_from_slice(channel_id);
        composite_key.extend_from_slice(&epoch.to_be_bytes());
        match self.get("room_post_epoch_key", &composite_key)? {
            Some(v) if v.len() == 32 => {
                let mut key = [0u8; 32];
                key.copy_from_slice(&v);
                Ok(Some(key))
            }
            _ => Ok(None),
        }
    }

    /// List all keys stored under a given `store_type`.
    pub fn list_keys(&self, store_type: &str) -> Result<Vec<Vec<u8>>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare("SELECT key FROM mls_state WHERE store_type = ?1")?;
        let keys = stmt
            .query_map(rusqlite::params![store_type], |row| row.get(0))?
            .collect::<std::result::Result<Vec<Vec<u8>>, _>>()?;
        Ok(keys)
    }

    pub fn put_dm_channel(&self, peer: &[u8; 32], channel: &[u8; 32]) -> Result<()> {
        let conn = self.conn()?;
        let now = fauna_core::data::Timestamp::now_secs();
        conn.execute(
            "INSERT OR REPLACE INTO dm_channels (peer_actor_id, channel_id, created_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![peer.as_slice(), channel.as_slice(), now],
        )?;
        Ok(())
    }

    pub fn get_dm_channel(&self, peer: &[u8; 32]) -> Result<Option<[u8; 32]>> {
        let conn = self.conn()?;
        let mut stmt =
            conn.prepare("SELECT channel_id FROM dm_channels WHERE peer_actor_id = ?1")?;
        let mut rows = stmt.query(rusqlite::params![peer.as_slice()])?;
        match rows.next()? {
            Some(row) => {
                let blob: Vec<u8> = row.get(0)?;
                if blob.len() == 32 {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(&blob);
                    Ok(Some(arr))
                } else {
                    Ok(None)
                }
            }
            None => Ok(None),
        }
    }

    /// Set the read timestamp for a peer conversation.
    pub fn set_read_timestamp(&self, peer: &[u8; 32], timestamp_ms: u64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT OR REPLACE INTO read_timestamps (peer_actor_id, last_read_ms) VALUES (?1, ?2)",
            rusqlite::params![peer.as_slice(), timestamp_ms as i64],
        )?;
        Ok(())
    }

    /// Get the read timestamp for a peer conversation. Returns 0 if never read.
    pub fn get_read_timestamp(&self, peer: &[u8; 32]) -> Result<u64> {
        let conn = self.conn()?;
        let result = conn.query_row(
            "SELECT last_read_ms FROM read_timestamps WHERE peer_actor_id = ?1",
            rusqlite::params![peer.as_slice()],
            |row| row.get::<_, i64>(0),
        );
        match result {
            Ok(ts) => Ok(ts as u64),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
            Err(e) => Err(e.into()),
        }
    }

    pub fn list_dm_channels(&self) -> Result<Vec<([u8; 32], [u8; 32])>> {
        let conn = self.conn()?;
        let mut stmt =
            conn.prepare("SELECT peer_actor_id, channel_id FROM dm_channels ORDER BY created_at")?;
        let rows = stmt.query_map([], |row| {
            let peer_blob: Vec<u8> = row.get(0)?;
            let ch_blob: Vec<u8> = row.get(1)?;
            Ok((peer_blob, ch_blob))
        })?;
        let mut result = Vec::new();
        for row in rows {
            let (peer_blob, ch_blob) = row?;
            if peer_blob.len() == 32 && ch_blob.len() == 32 {
                let mut peer = [0u8; 32];
                let mut ch = [0u8; 32];
                peer.copy_from_slice(&peer_blob);
                ch.copy_from_slice(&ch_blob);
                result.push((peer, ch));
            }
        }
        Ok(result)
    }

    pub fn put_channel_nest_url(&self, channel_id: &[u8; 32], nest_url: &str) -> Result<()> {
        self.conn()?.execute(
            "INSERT OR REPLACE INTO channel_nest_url (channel_id, nest_url) VALUES (?1, ?2)",
            rusqlite::params![&channel_id[..], nest_url],
        )?;
        Ok(())
    }

    pub fn get_channel_nest_url(&self, channel_id: &[u8; 32]) -> Result<Option<String>> {
        let conn = self.conn()?;
        let mut stmt =
            conn.prepare("SELECT nest_url FROM channel_nest_url WHERE channel_id = ?1")?;
        let result = stmt.query_row(rusqlite::params![&channel_id[..]], |row| {
            row.get::<_, String>(0)
        });
        match result {
            Ok(url) => Ok(Some(url)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn put_group_channel(&self, group_id: &str, channel: &[u8; 32]) -> Result<()> {
        let conn = self.conn()?;
        let now = fauna_core::data::Timestamp::now_secs();
        conn.execute(
            "INSERT OR REPLACE INTO group_channels (group_id, channel_id, created_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![group_id, channel.as_slice(), now],
        )?;
        Ok(())
    }

    pub fn get_group_channel(&self, group_id: &str) -> Result<Option<[u8; 32]>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare("SELECT channel_id FROM group_channels WHERE group_id = ?1")?;
        let mut rows = stmt.query(rusqlite::params![group_id])?;
        match rows.next()? {
            Some(row) => {
                let blob: Vec<u8> = row.get(0)?;
                if blob.len() == 32 {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(&blob);
                    Ok(Some(arr))
                } else {
                    Ok(None)
                }
            }
            None => Ok(None),
        }
    }

    /// Insert or replace an active group mapping (channel_id → mls_group_id).
    pub fn put_active_group(&self, channel_id: &[u8; 32], mls_group_id: &[u8]) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT OR REPLACE INTO active_groups (channel_id, mls_group_id) VALUES (?1, ?2)",
            rusqlite::params![channel_id.as_slice(), mls_group_id],
        )?;
        Ok(())
    }

    /// Remove an active group by channel_id.
    pub fn remove_active_group(&self, channel_id: &[u8; 32]) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM active_groups WHERE channel_id = ?1",
            rusqlite::params![channel_id.as_slice()],
        )?;
        Ok(())
    }

    /// List all active groups as (channel_id, mls_group_id) pairs.
    pub fn list_active_groups(&self) -> Result<Vec<([u8; 32], Vec<u8>)>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare("SELECT channel_id, mls_group_id FROM active_groups")?;
        let rows = stmt.query_map([], |row| {
            let ch_blob: Vec<u8> = row.get(0)?;
            let gid_blob: Vec<u8> = row.get(1)?;
            Ok((ch_blob, gid_blob))
        })?;
        let mut result = Vec::new();
        for row in rows {
            let (ch_blob, gid_blob) = row?;
            if ch_blob.len() == 32 {
                let mut ch = [0u8; 32];
                ch.copy_from_slice(&ch_blob);
                result.push((ch, gid_blob));
            }
        }
        Ok(result)
    }

    /// The engine's **last-seen replica listing** — the channels named by a
    /// replica listing this device adopted or authored, the ancestor the
    /// provider swap asks its join-or-deletion question against
    /// (`MlsEngine::note_replica_listed`). Rows of `mls_state` under their own
    /// `store_type`, so the record needs no schema step.
    pub fn list_replica_listed(&self) -> Result<Vec<[u8; 32]>> {
        Ok(self
            .list_keys(REPLICA_LISTED)?
            .into_iter()
            .filter_map(|k| <[u8; 32]>::try_from(k.as_slice()).ok())
            .collect())
    }

    /// Add channels to the last-seen replica listing.
    pub fn add_replica_listed(&self, channel_ids: &[[u8; 32]]) -> Result<()> {
        let conn = self.conn()?;
        let tx = conn.unchecked_transaction()?;
        for ch in channel_ids {
            tx.execute(
                "INSERT OR REPLACE INTO mls_state (store_type, key, value) VALUES (?1, ?2, X'01')",
                rusqlite::params![REPLICA_LISTED, ch.as_slice()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Remove one channel from the last-seen replica listing.
    pub fn remove_replica_listed(&self, channel_id: &[u8; 32]) -> Result<()> {
        self.delete(REPLICA_LISTED, channel_id)
    }

    /// Replace the whole last-seen replica listing, atomically.
    pub fn replace_replica_listed(&self, channel_ids: &[[u8; 32]]) -> Result<()> {
        let conn = self.conn()?;
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM mls_state WHERE store_type = ?1",
            rusqlite::params![REPLICA_LISTED],
        )?;
        for ch in channel_ids {
            tx.execute(
                "INSERT INTO mls_state (store_type, key, value) VALUES (?1, ?2, X'01')",
                rusqlite::params![REPLICA_LISTED, ch.as_slice()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Load all rows from the mls_state table as (store_type, key, value) triples.
    pub fn load_all_mls_state(&self) -> Result<Vec<MlsStateRow>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare("SELECT store_type, key, value FROM mls_state")?;
        let rows = stmt.query_map([], |row| {
            let st: String = row.get(0)?;
            let key: Vec<u8> = row.get(1)?;
            let val: Vec<u8> = row.get(2)?;
            Ok((st, key, val))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Save a serialized provider snapshot to the mls_state table.
    pub fn save_provider_snapshot(&self, data: &[u8]) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT OR REPLACE INTO mls_state (store_type, key, value) VALUES ('_provider_snapshot', X'00', ?1)",
            rusqlite::params![data],
        )?;
        Ok(())
    }

    /// Load a previously saved provider snapshot, if any.
    pub fn load_provider_snapshot(&self) -> Result<Option<Vec<u8>>> {
        self.get("_provider_snapshot", &[0x00])
    }

    pub fn list_group_channels(&self) -> Result<Vec<(String, [u8; 32])>> {
        let conn = self.conn()?;
        let mut stmt =
            conn.prepare("SELECT group_id, channel_id FROM group_channels ORDER BY created_at")?;
        let rows = stmt.query_map([], |row| {
            let gid: String = row.get(0)?;
            let ch_blob: Vec<u8> = row.get(1)?;
            Ok((gid, ch_blob))
        })?;
        let mut result = Vec::new();
        for row in rows {
            let (gid, ch_blob) = row?;
            if ch_blob.len() == 32 {
                let mut ch = [0u8; 32];
                ch.copy_from_slice(&ch_blob);
                result.push((gid, ch));
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod version_tests {
    use super::*;
    use crate::version::{CURRENT_SCHEMA_VERSION, MIN_READER_SCHEMA_VERSION, SchemaIncompatible};
    use tempfile::NamedTempFile;

    /// Build this store's current tables on `conn` — the test fixture for a
    /// database some build already wrote. Leaves it UNSTAMPED; tests that need
    /// a stamp add one with [`seed_schema_meta`].
    fn current_tables(conn: &Connection) {
        SqliteStorage::init_table(conn).unwrap();
    }

    /// Stamp an arbitrary `(schema_version, min_reader_version)` — used to forge
    /// a database "written by a newer build" without needing that build.
    fn seed_schema_meta(conn: &Connection, v: u16, min: u16) {
        conn.execute_batch(fauna_core::sqlite_schema_meta::SCHEMA_META_DDL)
            .unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO schema_meta (id, schema_version, min_reader_version, updated_at) \
             VALUES (1, ?1, ?2, 0)",
            rusqlite::params![v as i64, min as i64],
        )
        .unwrap();
    }

    fn read_stamp(conn: &Connection) -> (u16, u16) {
        conn.query_row(
            "SELECT schema_version, min_reader_version FROM schema_meta WHERE id = 1",
            [],
            |r| Ok((r.get::<_, i64>(0)? as u16, r.get::<_, i64>(1)? as u16)),
        )
        .unwrap()
    }

    /// An unstamped `mls.db` that already holds tables is refused, and the
    /// refusal writes nothing — not even the stamp. Only a build predating the
    /// schema-version scheme wrote such a file; its adoption (read as the
    /// baseline, then stamped) was retired by the compat-remnant sweep
    /// (`version-compatibility.md` § Dimension 2, program 4).
    #[test]
    fn an_unstamped_db_holding_tables_is_refused_and_left_untouched() {
        let tmp = NamedTempFile::new().unwrap();
        {
            let conn = Connection::open(tmp.path()).unwrap();
            current_tables(&conn);
            conn.execute_batch("DROP TABLE schema_meta;").unwrap();
            conn.execute(
                "INSERT INTO mls_state (store_type, key, value) VALUES ('group_state', X'01', X'DEADBEEF')",
                [],
            )
            .unwrap();
        }

        let err = SqliteStorage::open(tmp.path())
            .expect_err("an unstamped db holding tables is a pre-scheme file — refused");
        assert!(
            format!("{err:#}").contains("no schema_meta stamp"),
            "the refusal must say why, got: {err:#}",
        );

        let conn = Connection::open(tmp.path()).unwrap();
        let has_meta: bool = conn
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE name = 'schema_meta')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!has_meta, "a refused open must not create schema_meta");
        let value: Vec<u8> = conn
            .query_row(
                "SELECT value FROM mls_state WHERE store_type = 'group_state' AND key = X'01'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(value, vec![0xDE, 0xAD, 0xBE, 0xEF]);
    }

    /// I2 backward-compat: a database a *newer* build wrote, but only additively
    /// (its reader floor is still within us). We operate on it normally — and we
    /// must not restamp its version *down*, which would lie to the newer build.
    #[test]
    fn newer_additive_db_operates_and_is_not_restamped_down() {
        let tmp = NamedTempFile::new().unwrap();
        {
            let conn = Connection::open(tmp.path()).unwrap();
            current_tables(&conn);
            seed_schema_meta(&conn, CURRENT_SCHEMA_VERSION + 5, MIN_READER_SCHEMA_VERSION);
        }

        let storage = SqliteStorage::open(tmp.path())
            .expect("a newer-but-additive db is readable (I2 backward-compat)");
        storage.put("group_state", b"k", b"v").unwrap();

        let conn = Connection::open(tmp.path()).unwrap();
        let (v, min) = read_stamp(&conn);
        assert_eq!(
            v,
            CURRENT_SCHEMA_VERSION + 5,
            "an older binary must not restamp a newer db's version down",
        );
        assert_eq!(min, MIN_READER_SCHEMA_VERSION, "nor lower its reader floor");
    }

    /// The I1 assertion this whole track exists for: a database carrying a
    /// **breaking** change we predate is refused *honestly* — and refusing it
    /// must not touch a single byte of the user's irrecoverable MLS state.
    #[test]
    fn newer_breaking_db_is_an_honest_error_and_destroys_nothing() {
        let tmp = NamedTempFile::new().unwrap();
        {
            let conn = Connection::open(tmp.path()).unwrap();
            current_tables(&conn);
            conn.execute(
                "INSERT INTO mls_state (store_type, key, value) VALUES ('group_state', X'01', X'DEADBEEF')",
                [],
            )
            .unwrap();
            seed_schema_meta(
                &conn,
                CURRENT_SCHEMA_VERSION + 5,
                CURRENT_SCHEMA_VERSION + 5,
            );
        }

        let err = SqliteStorage::open(tmp.path())
            .expect_err("a newer-breaking db must not be opened and silently misread");
        let typed = err
            .downcast_ref::<SchemaIncompatible>()
            .expect("the refusal must be the typed verdict, not an opaque sqlite error");
        assert_eq!(typed.db_v, CURRENT_SCHEMA_VERSION + 5);
        assert_eq!(typed.db_min, CURRENT_SCHEMA_VERSION + 5);
        assert_eq!(typed.bin_v, CURRENT_SCHEMA_VERSION);

        // I1: the data is still there, and the stamp was not rewritten.
        let conn = Connection::open(tmp.path()).unwrap();
        let value: Vec<u8> = conn
            .query_row(
                "SELECT value FROM mls_state WHERE store_type = 'group_state' AND key = X'01'",
                [],
                |r| r.get(0),
            )
            .expect("refusing to open must never destroy the user's MLS state");
        assert_eq!(value, vec![0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(
            read_stamp(&conn),
            (CURRENT_SCHEMA_VERSION + 5, CURRENT_SCHEMA_VERSION + 5),
            "an incompatible db must not be mutated at all — not even its stamp",
        );
    }

    /// The reconcile: a column the current DDL declares but an older database
    /// lacks is added in place, without disturbing existing rows. Without this,
    /// the first write after a schema growth fails lazily at runtime against an
    /// N−1 database.
    #[test]
    fn reconcile_adds_a_missing_defaulted_column_preserving_rows() {
        let tmp = NamedTempFile::new().unwrap();
        {
            let conn = Connection::open(tmp.path()).unwrap();
            current_tables(&conn);
            seed_schema_meta(&conn, CURRENT_SCHEMA_VERSION, MIN_READER_SCHEMA_VERSION);
            // Simulate an older shape: send_sequences without `next_seq`.
            conn.execute_batch(
                "DROP TABLE send_sequences;
                 CREATE TABLE send_sequences (channel_id BLOB NOT NULL PRIMARY KEY);
                 INSERT INTO send_sequences (channel_id) VALUES (X'AA');",
            )
            .unwrap();
        }

        let storage = SqliteStorage::open(tmp.path())
            .expect("a db missing an additive column must be reconciled, not rejected");

        // The pre-existing row survived and took the column's declared default.
        let conn = Connection::open(tmp.path()).unwrap();
        let seq: i64 = conn
            .query_row(
                "SELECT next_seq FROM send_sequences WHERE channel_id = X'AA'",
                [],
                |r| r.get(0),
            )
            .expect("the reconciled column must exist on the old row");
        assert_eq!(seq, 1, "the row must take the DDL's declared default");
        drop(conn);

        // And the column is actually usable through the normal write path.
        storage.next_send_sequence(&[0xBB; 32]).unwrap();
    }

    /// A `NOT NULL`-without-default column cannot be added by `ALTER TABLE`.
    /// That is a non-additive change needing an explicit rebuild migration, so
    /// it must be a loud error — never a silent skip that fails later at insert.
    #[test]
    fn missing_notnull_column_without_default_is_loud() {
        let tmp = NamedTempFile::new().unwrap();
        {
            let conn = Connection::open(tmp.path()).unwrap();
            current_tables(&conn);
            seed_schema_meta(&conn, CURRENT_SCHEMA_VERSION, MIN_READER_SCHEMA_VERSION);
            conn.execute_batch(
                "DROP TABLE dm_channels;
                 CREATE TABLE dm_channels (
                     peer_actor_id BLOB NOT NULL PRIMARY KEY,
                     channel_id BLOB NOT NULL
                 );",
            )
            .unwrap();
        }

        let err = SqliteStorage::open(tmp.path())
            .expect_err("a NOT NULL column with no default cannot be auto-added");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("dm_channels.created_at"),
            "the error must name the offending column, got: {msg}",
        );
    }

    #[test]
    fn fresh_db_is_stamped_at_current_version() {
        let tmp = NamedTempFile::new().unwrap();
        SqliteStorage::open(tmp.path()).unwrap();
        let conn = Connection::open(tmp.path()).unwrap();
        assert_eq!(
            read_stamp(&conn),
            (CURRENT_SCHEMA_VERSION, MIN_READER_SCHEMA_VERSION)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn open_and_basic_operations() {
        let tmp = NamedTempFile::new().unwrap();
        let storage = SqliteStorage::open(tmp.path()).unwrap();

        // Verify the table exists by doing a simple put+get (autocommit).
        storage.put("test", b"k1", b"v1").unwrap();
        let val = storage.get("test", b"k1").unwrap();
        assert_eq!(val, Some(b"v1".to_vec()));

        // Re-open the database and confirm data persisted (autocommit).
        drop(storage);
        let storage2 = SqliteStorage::open(tmp.path()).unwrap();
        let val = storage2.get("test", b"k1").unwrap();
        assert_eq!(val, Some(b"v1".to_vec()));
    }

    #[test]
    fn put_get_delete_roundtrip() {
        let tmp = NamedTempFile::new().unwrap();
        let storage = SqliteStorage::open(tmp.path()).unwrap();

        // Put a value and read it back.
        storage.put("state", b"key1", b"hello").unwrap();
        assert_eq!(
            storage.get("state", b"key1").unwrap(),
            Some(b"hello".to_vec()),
        );

        // Overwrite with a new value.
        storage.put("state", b"key1", b"world").unwrap();
        assert_eq!(
            storage.get("state", b"key1").unwrap(),
            Some(b"world".to_vec()),
        );

        // Delete it.
        storage.delete("state", b"key1").unwrap();
        assert_eq!(storage.get("state", b"key1").unwrap(), None);

        // Deleting a non-existent key is a no-op.
        storage.delete("state", b"key1").unwrap();
    }

    #[test]
    fn list_keys_by_type() {
        let tmp = NamedTempFile::new().unwrap();
        let storage = SqliteStorage::open(tmp.path()).unwrap();

        // Insert 2 keys under "alpha" and 1 under "beta".
        storage.put("alpha", b"a1", b"val").unwrap();
        storage.put("alpha", b"a2", b"val").unwrap();
        storage.put("beta", b"b1", b"val").unwrap();

        let mut alpha_keys = storage.list_keys("alpha").unwrap();
        alpha_keys.sort();
        assert_eq!(alpha_keys, vec![b"a1".to_vec(), b"a2".to_vec()]);

        let beta_keys = storage.list_keys("beta").unwrap();
        assert_eq!(beta_keys, vec![b"b1".to_vec()]);

        // A type with no keys returns an empty list.
        let empty = storage.list_keys("gamma").unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn send_sequence_tracking() {
        let store = SqliteStorage::open_in_memory().unwrap();
        let channel = [1u8; 32];

        assert_eq!(store.next_send_sequence(&channel).unwrap(), 1);
        assert_eq!(store.next_send_sequence(&channel).unwrap(), 2);
        assert_eq!(store.next_send_sequence(&channel).unwrap(), 3);
        // Different channel starts at 1
        assert_eq!(store.next_send_sequence(&[2u8; 32]).unwrap(), 1);
    }

    #[test]
    fn recv_sequence_tracking() {
        let store = SqliteStorage::open_in_memory().unwrap();
        let channel = [1u8; 32];
        let sender = [0xAAu8; 32];

        assert_eq!(store.last_recv_sequence(&channel, &sender).unwrap(), 0);
        store.set_recv_sequence(&channel, &sender, 5).unwrap();
        assert_eq!(store.last_recv_sequence(&channel, &sender).unwrap(), 5);
        store.set_recv_sequence(&channel, &sender, 10).unwrap();
        assert_eq!(store.last_recv_sequence(&channel, &sender).unwrap(), 10);
    }

    #[test]
    fn dm_channel_put_get_roundtrip() {
        let store = SqliteStorage::open_in_memory().unwrap();
        let peer = [0xAAu8; 32];
        let channel = [0xBBu8; 32];
        assert_eq!(store.get_dm_channel(&peer).unwrap(), None);
        store.put_dm_channel(&peer, &channel).unwrap();
        assert_eq!(store.get_dm_channel(&peer).unwrap(), Some(channel));
        let channel2 = [0xCCu8; 32];
        store.put_dm_channel(&peer, &channel2).unwrap();
        assert_eq!(store.get_dm_channel(&peer).unwrap(), Some(channel2));
    }

    #[test]
    fn read_timestamp_roundtrip() {
        let store = SqliteStorage::open_in_memory().unwrap();
        let peer = [0xAAu8; 32];

        // Default is 0 (never read).
        assert_eq!(store.get_read_timestamp(&peer).unwrap(), 0);

        // Set and get.
        store.set_read_timestamp(&peer, 12345).unwrap();
        assert_eq!(store.get_read_timestamp(&peer).unwrap(), 12345);

        // Update.
        store.set_read_timestamp(&peer, 99999).unwrap();
        assert_eq!(store.get_read_timestamp(&peer).unwrap(), 99999);
    }

    #[test]
    fn dm_channel_list_returns_all() {
        let store = SqliteStorage::open_in_memory().unwrap();
        let peer_a = [1u8; 32];
        let peer_b = [2u8; 32];
        let ch_a = [0xA0u8; 32];
        let ch_b = [0xB0u8; 32];
        store.put_dm_channel(&peer_a, &ch_a).unwrap();
        store.put_dm_channel(&peer_b, &ch_b).unwrap();
        let mut channels = store.list_dm_channels().unwrap();
        channels.sort_by_key(|(p, _)| *p);
        assert_eq!(channels.len(), 2);
        assert_eq!(channels[0], (peer_a, ch_a));
        assert_eq!(channels[1], (peer_b, ch_b));
    }

    #[test]
    fn channel_nest_url_roundtrip() {
        let storage = SqliteStorage::open_in_memory().unwrap();
        let channel_id = [0xAA; 32];

        assert!(storage.get_channel_nest_url(&channel_id).unwrap().is_none());

        storage
            .put_channel_nest_url(&channel_id, "http://127.0.0.1:3000")
            .unwrap();
        assert_eq!(
            storage
                .get_channel_nest_url(&channel_id)
                .unwrap()
                .as_deref(),
            Some("http://127.0.0.1:3000"),
        );

        storage
            .put_channel_nest_url(&channel_id, "http://127.0.0.1:4000")
            .unwrap();
        assert_eq!(
            storage
                .get_channel_nest_url(&channel_id)
                .unwrap()
                .as_deref(),
            Some("http://127.0.0.1:4000"),
        );
    }

    #[test]
    fn group_channel_put_get_roundtrip() {
        let store = SqliteStorage::open_in_memory().unwrap();
        assert_eq!(store.get_group_channel("group-1").unwrap(), None);
        let channel = [0xAAu8; 32];
        store.put_group_channel("group-1", &channel).unwrap();
        assert_eq!(store.get_group_channel("group-1").unwrap(), Some(channel));

        let channel2 = [0xBBu8; 32];
        store.put_group_channel("group-2", &channel2).unwrap();
        let all = store.list_group_channels().unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn active_group_put_list_remove() {
        let store = SqliteStorage::open_in_memory().unwrap();
        assert!(store.list_active_groups().unwrap().is_empty());
        let ch1 = [0xAAu8; 32];
        let gid1 = b"mls-group-id-1".to_vec();
        store.put_active_group(&ch1, &gid1).unwrap();
        let ch2 = [0xBBu8; 32];
        let gid2 = b"mls-group-id-2".to_vec();
        store.put_active_group(&ch2, &gid2).unwrap();
        let groups = store.list_active_groups().unwrap();
        assert_eq!(groups.len(), 2);
        store.remove_active_group(&ch1).unwrap();
        let groups = store.list_active_groups().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].0, ch2);
        assert_eq!(groups[0].1, gid2);
    }

    #[test]
    fn active_group_upsert() {
        let store = SqliteStorage::open_in_memory().unwrap();
        let ch = [0xAAu8; 32];
        store.put_active_group(&ch, b"first".as_slice()).unwrap();
        store.put_active_group(&ch, b"second".as_slice()).unwrap();
        let groups = store.list_active_groups().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].1, b"second".to_vec());
    }

    #[test]
    fn load_all_mls_state_roundtrip() {
        let store = SqliteStorage::open_in_memory().unwrap();
        store.put("group_state", b"key1", b"val1").unwrap();
        store.put("epoch_secret", b"key2", b"val2").unwrap();
        let all = store.load_all_mls_state().unwrap();
        assert_eq!(all.len(), 2);
        let entry1 = all
            .iter()
            .find(|(st, k, _)| st == "group_state" && k == b"key1")
            .unwrap();
        assert_eq!(entry1.2, b"val1".to_vec());
    }

    /// The conversations-engine role exclusion pin: a second open of the same
    /// database is refused with the **typed** [`StateServedElsewhere`] while
    /// the first storage lives, and succeeds once it drops. Two same-process
    /// opens contend exactly like two processes — the lock lives on the open
    /// file description — which is also what makes "one engine over one
    /// `mls_state.db`, never two" kernel-enforced rather than a comment.
    #[test]
    fn a_second_open_of_the_same_database_is_refused_while_the_first_lives() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("mls_state.db");

        let first = SqliteStorage::open(&db).unwrap();
        let err = SqliteStorage::open(&db).expect_err("a live holder must refuse a second open");
        let served = err
            .downcast_ref::<StateServedElsewhere>()
            .expect("the refusal must be the typed StateServedElsewhere, not a generic failure");
        assert!(served.path.contains("mls_state.db"));

        drop(first);
        SqliteStorage::open(&db).expect("release must free the role for the next open");
    }

    /// The hand-over releases the DATABASE, not merely the role lock and the right
    /// to issue statements. That distinction is invisible on POSIX — `unlink`
    /// deletes an open file — and total on Windows, where an open handle makes the
    /// file undeletable (`ERROR_SHARING_VIOLATION`, `os error 32`). A sign-out's
    /// erase is exactly this `remove_dir_all` over the actor scope, so for months a
    /// signed-out user's `mls.db` survived the erase meant to remove it, on Windows
    /// only.
    ///
    /// Both halves are asserted on purpose: the structural one is the mechanism and
    /// holds everywhere, and the behavioural one is the property a user actually
    /// gets — vacuous on POSIX, and the whole bug on Windows.
    #[test]
    fn retire_closes_the_database_so_the_account_scope_can_be_removed() {
        let dir = tempfile::tempdir().unwrap();
        let scope = dir.path().join("32bb47af63e2b44c");
        std::fs::create_dir_all(&scope).unwrap();
        let db = scope.join("mls.db");
        let store = SqliteStorage::open(&db).unwrap();
        assert!(db.exists(), "the open must have created the database");

        store.retire();

        assert!(
            store.conn.lock().expect("lock poisoned").is_none(),
            "retire must CLOSE the connection, not only refuse later statements: \
             refusing keeps the handle, and the handle is what pins the file"
        );
        std::fs::remove_dir_all(&scope).expect(
            "the actor scope must be removable straight after a retire — this is \
             literally the erase a sign-out performs (erase_all_account_scopes → \
             remove_dir_all), and an open connection fails it os error 32",
        );
        assert!(!scope.exists());
    }

    /// The hand-over half of the same pin: [`SqliteStorage::retire`] frees the
    /// role for a successor **without waiting for the predecessor to drop**, and
    /// the retired handle then refuses every statement.
    ///
    /// Both halves matter and neither alone is the fix. Freeing without refusing
    /// would leave two live writers over one `mls_state.db` — the permanent
    /// ratchet-fork hazard the role lock exists for. Refusing without freeing
    /// would leave the successor locked out, which is the defect being fixed.
    #[test]
    fn retire_frees_the_role_for_a_successor_and_refuses_the_retired_handle() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("mls_state.db");

        let previous = SqliteStorage::open(&db).unwrap();
        previous
            .put("group_state", b"k", b"v")
            .expect("live writes");

        previous.retire();

        // The successor gets the role while the predecessor is still very much
        // alive — the whole point, since the predecessor's `Arc` can be held by a
        // shell field no Rust code can reach.
        let successor =
            SqliteStorage::open(&db).expect("retire must free the role for the successor");
        successor
            .put("group_state", b"k2", b"v2")
            .expect("the successor owns the store");

        let err = previous
            .put("group_state", b"k3", b"v3")
            .expect_err("a retired handle must not write the store it handed over");
        let retired = err
            .downcast_ref::<MlsStateRetired>()
            .expect("the refusal must be the typed MlsStateRetired, not a generic failure");
        assert!(retired.path.contains("mls_state.db"));
        assert!(
            previous.get("group_state", b"k").is_err(),
            "reads are refused too — a retired handle serves a store it no longer owns"
        );

        // Idempotent: a second retire must not panic or disturb the successor.
        previous.retire();
        successor
            .get("group_state", b"k2")
            .expect("the successor is unaffected by the predecessor's second retire");
    }

    /// The lock file is the database's own name + `.lock`, minted beside it,
    /// and **never deleted on release** (unlinking re-opens the race).
    #[test]
    fn the_role_lock_is_named_after_the_database_and_release_never_deletes_it() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("mls_state.db");

        let store = SqliteStorage::open(&db).unwrap();
        let lock = role_lock_path(&db);
        assert!(lock.ends_with("mls_state.db.lock"));
        assert!(
            lock.exists(),
            "open mints the lock file beside the database"
        );
        drop(store);
        assert!(
            lock.exists(),
            "release must not unlink the lock file (unlinking re-opens the race)"
        );
    }

    /// Two *different* databases sharing one directory do not contend — the
    /// lock name is derived from the guarded file, not fixed per directory.
    /// This is the pin on the 2026-08-15 build refinement: a fixed sibling
    /// name (`mls.lock`) would have made every `NamedTempFile`-shaped test in
    /// the workspace — and any future two-databases-one-directory layout —
    /// contend on a single machine-global lock in the shared OS temp dir.
    #[test]
    fn two_databases_in_one_directory_do_not_contend() {
        let dir = tempfile::tempdir().unwrap();
        let _a = SqliteStorage::open(&dir.path().join("a.db")).unwrap();
        let _b = SqliteStorage::open(&dir.path().join("b.db"))
            .expect("a sibling database's role is independent — its lock is its own file");
        assert_ne!(
            role_lock_path(&dir.path().join("a.db")),
            role_lock_path(&dir.path().join("b.db")),
        );
    }

    /// The ratified tripwire (2026-08-15): the connection stays on the
    /// rollback journal with `busy_timeout` 0, so a role-guard regression's
    /// first symptom is a hard `SQLITE_BUSY` — loud — never silent WAL
    /// interleaving of two engines' writes. Do not "modernize" these pragmas;
    /// see `prepare`'s doc comment.
    #[test]
    fn the_ratified_tripwire_non_wal_journal_and_zero_busy_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteStorage::open(&dir.path().join("mls_state.db")).unwrap();
        let conn = store.conn().unwrap();
        let journal: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_ne!(
            journal.to_ascii_lowercase(),
            "wal",
            "mls_state.db must stay off WAL — the ratified tripwire"
        );
        let busy: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .unwrap();
        assert_eq!(busy, 0, "busy_timeout must stay 0 — the ratified tripwire");
    }
}
