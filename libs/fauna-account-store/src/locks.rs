//! The store's advisory locks (W5 (account-data-plane.md § Workstreams) — charter:
//! `docs/goal/architecture/account-runtime.md` § Multi-instance
//! concurrency).
//!
//! The charter's store contract names **three genuinely exclusive critical
//! sections** for concurrent same-account processes sharing one store dir,
//! and this module owns them all, so a cold read finds them together. Beside
//! them sit two **presence** locks, which exclude nobody from the store: one
//! so an erase can ask who is running out of it, one so an app can ask
//! whether the sync agent hosts it:
//!
//! | Lock | Section | Shape |
//! |---|---|---|
//! | [`EngineLock`] (W5.1) | the **engine-singleton role** — one process runs the sync engine + outbox drain | **try**-acquire, held for the role's lifetime |
//! | [`SeedLegLock`] | the **seed-leg role** — one seed-holding process runs the legs only a signed-in app can run (escrow recovery, the linked-nest secondary leg), in its pass when it also pumps and in a seed pass beside a seedless engine holder | **try**-acquire, held for the role's lifetime; a seedless process never asks |
//! | [`MigrationLock`] (W5.3) | **schema migration/adoption** at store open | **blocking** acquire, held only across the section |
//! | [`ServingLock`] | **presence, not exclusion** — "an app instance is serving this account out of this root" | **shared** for a serving instance's lifetime; a momentary **exclusive try** is the erase's question |
//! | [`AgentPresenceLock`] | **presence, not exclusion** — "the sync agent hosts this store", which gives it the engine role over any app | **exclusive, blocking** for the agent mount's lifetime; a momentary **shared try** is a seed-holding app's question — [`ServingLock`]'s sides swapped |
//!
//! So the count of exclusive sections is unchanged by the last two rows: three
//! in the store, and the conversations-engine role outside it. The serving lock
//! is the cross-app twin of `fauna_client_accounts::AccountInstanceLock`, keyed
//! on the one directory every app on an OS login shares instead of on each
//! app's own install base (`account-scoping.md` § Concurrent instances → *An
//! erase refuses while a sibling serves the account*).
//!
//! The shapes differ because the questions differ. The role is *won*: a
//! non-holder has a full life as a plain reader/writer, so there is nothing
//! to wait for and `try_acquire` reports [`Refused`](EngineLockOutcome::Refused)
//! instead of queuing. Migration is *passed through*: every opener needs the
//! store migrated before it can use it, so waiting is the whole point — the
//! second opener blocks, then finds the work already done and skips it.
//!
//! Everything else is common to both, and is the shipped idiom of
//! `fauna_client_accounts::AccountInstanceLock` (the (OS login, account)
//! refusal lock the role-election narrows at W5.6) and the sync agent's
//! socket `InstanceLock` (`apps/sync-agent.md` § single-instance — T9 names
//! it as the precedent):
//!
//! - **Crash-safe by construction.** `flock` on unix, `LockFileEx` on
//!   windows, via std's `File::lock`/`try_lock` — the kernel releases when
//!   the holder dies, so there is no stale-lock reconciliation at next start
//!   and a process that dies mid-migration strands nobody: the next opener
//!   acquires and completes it.
//! - **The lock file is never deleted.** Unlinking a lock file re-opens the
//!   race it closes (a new acquirer can lock the orphaned inode while
//!   another creates a fresh file at the same path). Each is a zero-byte
//!   `0600` file whose name — not its content — is the key, which is why
//!   `fauna-account-store` reserves the names
//!   ([`ENGINE_LOCK_FILENAME`](crate::store::ENGINE_LOCK_FILENAME),
//!   [`SEED_LEGS_LOCK_FILENAME`](crate::store::SEED_LEGS_LOCK_FILENAME),
//!   [`MIGRATION_LOCK_FILENAME`](crate::store::MIGRATION_LOCK_FILENAME)) even
//!   for builds that never elect.
//! - **Policy stays with the caller.** An I/O failure yields `Degraded` with
//!   the error; what a degrade *means* (pump anyway at start; migrate anyway
//!   at open) is the caller's ruling, documented at its match site — this
//!   module only reports what the kernel said.
//!
//! Two same-process acquires contend exactly like two processes: a lock lives
//! on the *open file description*, and each acquire opens its own — which is
//! what lets the two-runtimes-one-store-dir proofs run in one test process.
//!
//! **Leg status:** unix is the W5.1/W5.3 leg, proven here and in the
//! runtime's conformance suite. The code compiles on windows (std maps both
//! calls to `LockFileEx`), but T9 ratifies a *named mutex derived from the
//! store path* for windows; whether `LockFileEx` on these same files is the
//! simpler uniform shape is the windows leg's call, graded on Windows — until
//! then only unix is proven. Web has its own leg under this same module path
//! (`locks_web.rs`, mounted as `crate::locks` on wasm32): the two role locks
//! over the Web Locks API, among tabs, with the same [`EngineLock`] /
//! [`EngineLockOutcome`] and [`SeedLegLock`] / [`SeedLegLockOutcome`] names.

use std::fs::File;
use std::path::{Path, PathBuf};

use crate::store::{agent_lock_path, engine_lock_path, migration_lock_path, seed_legs_lock_path};

/// The serving lock's path for one actor —
/// `<store root>/serving-<actor-id-hex>.lock`, reserved at the store **root**
/// ([`crate::root::StoreRoot::base`]).
///
/// ⚠ **At the root, never inside `<root>/<actor>/`.** An account erase
/// `remove_dir_all`s that directory ([`crate::db::erase_actor_state`]), so a
/// guard file inside it would be unlinked by the very erase it guards — the
/// race [`fauna_core::fs_lock`]'s "never delete a lock file" exists to close.
/// At the root it is a sibling of the actor dirs that no sweep names:
/// [`crate::db::erase_all_account_scopes`] removes only well-formed 64-hex
/// *directories*, and this is neither. (The two exclusive locks above DO live
/// inside the actor's store dir; that is sound for them only because the
/// serving lock is what keeps an erase from running while anyone holds them.)
///
/// Junk hex is refused through the shared actor-scope floor rather than minted
/// as a stray file — the rule every other per-actor path obeys.
pub fn serving_lock_path(store_root: &Path, actor_id_hex: &str) -> anyhow::Result<PathBuf> {
    let scope = crate::db::actor_state_dir(Path::new(""), actor_id_hex)?;
    Ok(store_root.join(format!("serving-{}.lock", scope.display())))
}

/// Result of [`EngineLock::try_acquire`].
#[derive(Debug)]
pub enum EngineLockOutcome {
    /// This process now holds the engine-singleton role; the lock releases
    /// when the value drops (or the process dies).
    Held(EngineLock),
    /// Another live open file description holds the role — usually another
    /// process, possibly another runtime in this one. Run as a plain
    /// reader/writer and re-try on the backstop cadence.
    Refused,
    /// The lock could not be taken for a reason other than a live holder.
    /// The caller owns what this means (see module docs).
    Degraded(std::io::Error),
}

/// Held engine-singleton election lock; RAII — dropping releases.
#[derive(Debug)]
pub struct EngineLock {
    /// Held (and thereby locked) until drop; never read.
    _file: File,
}

impl EngineLock {
    /// Try to become the engine singleton for the store rooted at
    /// `store_dir`. Never blocks; creates `store_dir` (and the lock file)
    /// if missing, so acquisition order relative to the store's own open is
    /// the caller's choice.
    pub fn try_acquire(store_dir: &Path) -> EngineLockOutcome {
        let file = match open_lock_file(store_dir, &engine_lock_path(store_dir)) {
            Ok(f) => f,
            Err(e) => return EngineLockOutcome::Degraded(e),
        };
        match file.try_lock() {
            Ok(()) => EngineLockOutcome::Held(Self { _file: file }),
            Err(std::fs::TryLockError::WouldBlock) => EngineLockOutcome::Refused,
            Err(std::fs::TryLockError::Error(e)) => EngineLockOutcome::Degraded(e),
        }
    }
}

/// Result of [`SeedLegLock::try_acquire`] — [`EngineLockOutcome`]'s arms, for
/// the store's other role.
#[derive(Debug)]
pub enum SeedLegLockOutcome {
    /// This process now holds the seed-leg role; the lock releases when the
    /// value drops (or the process dies).
    Held(SeedLegLock),
    /// Another live open file description holds the role — another
    /// seed-holding process, or another runtime in this one. Run no seed-only
    /// leg and re-try on the backstop cadence.
    Refused,
    /// The lock could not be taken for a reason other than a live holder.
    /// The caller owns what this means (see module docs).
    Degraded(std::io::Error),
}

/// Held seed-leg role lock; RAII — dropping releases.
#[derive(Debug)]
pub struct SeedLegLock {
    /// Held (and thereby locked) until drop; never read.
    _file: File,
}

impl SeedLegLock {
    /// Try to become the store's seed-leg holder
    /// (`account-runtime.md` § Multi-instance concurrency → *The seed-leg
    /// role*). The engine election's own mechanics on a file of its own
    /// ([`seed_legs_lock_path`]): never blocks, creates what is missing, no
    /// priority — the first seed holder to take it keeps it until it exits.
    /// Only a seed-holding runtime calls this.
    pub fn try_acquire(store_dir: &Path) -> SeedLegLockOutcome {
        let file = match open_lock_file(store_dir, &seed_legs_lock_path(store_dir)) {
            Ok(f) => f,
            Err(e) => return SeedLegLockOutcome::Degraded(e),
        };
        match file.try_lock() {
            Ok(()) => SeedLegLockOutcome::Held(Self { _file: file }),
            Err(std::fs::TryLockError::WouldBlock) => SeedLegLockOutcome::Refused,
            Err(std::fs::TryLockError::Error(e)) => SeedLegLockOutcome::Degraded(e),
        }
    }
}

/// Result of [`MigrationLock::acquire`].
///
/// There is deliberately **no `Refused`**: the acquire blocks, so a live
/// holder is a wait, not an outcome. A caller that could usefully do
/// something else while another process migrates does not exist — the store
/// is unusable until the section completes.
#[derive(Debug)]
pub enum MigrationLockOutcome {
    /// This process is inside the migration/adoption critical section; the
    /// lock releases when the value drops (or the process dies).
    Held(MigrationLock),
    /// The lock could not be taken for a reason other than a live holder —
    /// a read-only or otherwise unusable store dir. The caller owns what
    /// this means (see [`crate::sqlite::SqliteBackend::open`], which
    /// migrates anyway: degrading OPEN is the shipped `AccountInstanceLock`
    /// posture, and the single-process case — every case before W5 — is
    /// exactly the one a degrade lands in).
    Degraded(std::io::Error),
}

/// Held migration/adoption critical-section lock; RAII — dropping releases.
#[derive(Debug)]
pub struct MigrationLock {
    /// Held (and thereby locked) until drop; never read.
    _file: File,
}

impl MigrationLock {
    /// Enter the migration/adoption critical section for the store rooted at
    /// `store_dir`, **blocking** until any other opener leaves it.
    ///
    /// The wait is bounded by the other process's migration, and by nothing
    /// else: the lock is held across the section only, never for the
    /// lifetime of the store handle, so a long-lived reader can never make a
    /// cold opener wait. A holder that dies mid-section releases at the
    /// kernel, and the next acquirer completes the migration it abandoned —
    /// which is why the migrations themselves must stay re-runnable.
    ///
    /// **Never hold this across an `.await`.** It blocks the calling
    /// *thread*, so a task parked inside it holds a runtime worker; two
    /// tasks racing it on a current-thread runtime would deadlock outright.
    /// Both callers today are fully synchronous while they hold it
    /// ([`SqliteBackend::open`](crate::sqlite::SqliteBackend::open) and the
    /// runtime's writer-key resolution), which is what keeps that safe —
    /// keep any new section synchronous too.
    pub fn acquire(store_dir: &Path) -> MigrationLockOutcome {
        let file = match open_lock_file(store_dir, &migration_lock_path(store_dir)) {
            Ok(f) => f,
            Err(e) => return MigrationLockOutcome::Degraded(e),
        };
        match file.lock() {
            Ok(()) => MigrationLockOutcome::Held(Self { _file: file }),
            Err(e) => MigrationLockOutcome::Degraded(e),
        }
    }
}

/// Result of [`ServingLock::acquire`].
///
/// There is deliberately **no `Refused`**: serving is never exclusive across
/// apps — tui beside linux on one account was never refused and this lock must
/// not start refusing it — so a live holder is company, not an outcome.
#[derive(Debug)]
pub enum ServingLockOutcome {
    /// This instance is now visibly serving the account; the lock releases
    /// when the value drops (or the process dies).
    Held(ServingLock),
    /// The lock could not be taken. The caller serves anyway, unseen by the
    /// next erase's probe — degrade **open**, the posture of the instance lock
    /// this one twins: a lock-file hiccup must not become an account that
    /// cannot be opened.
    Degraded(anyhow::Error),
}

/// Held account serving lock; RAII — dropping releases.
#[derive(Debug)]
pub struct ServingLock {
    /// Held (and thereby share-locked) until drop; never read.
    _file: File,
}

impl ServingLock {
    /// Declare that this app instance serves `actor_id_hex` out of
    /// `store_root`, for as long as the returned lock lives.
    ///
    /// A **blocking shared** acquire, and the wait is bounded by construction:
    /// the only exclusive taker is [`Self::is_served`], which holds for the
    /// length of one `try_lock` and drops. A `try` here would instead turn that
    /// momentary probe into a serving instance nobody can see.
    pub fn acquire(store_root: &Path, actor_id_hex: &str) -> ServingLockOutcome {
        let file = match open_serving_lock_file(store_root, actor_id_hex) {
            Ok(f) => f,
            Err(e) => return ServingLockOutcome::Degraded(e),
        };
        match file.lock_shared() {
            Ok(()) => ServingLockOutcome::Held(Self { _file: file }),
            Err(e) => ServingLockOutcome::Degraded(e.into()),
        }
    }

    /// **Is any app instance serving `actor_id_hex` out of `store_root`?** —
    /// the question an erase asks before it unlinks anything.
    ///
    /// The exclusive take is the arbiter: winning it proves no shared holder
    /// exists, and it is dropped at once, restoring what the caller found. A
    /// caller that itself holds a [`ServingLock`] on this account must put it
    /// down first or it will see its own reflection
    /// (`fauna_client_accounts::SessionInstanceHolder::without_own_lock` does).
    ///
    /// A probe that cannot reach the file answers **not served** — degrade
    /// open, because the alternative is a device its owner cannot sign out of.
    pub fn is_served(store_root: &Path, actor_id_hex: &str) -> bool {
        let Ok(file) = open_serving_lock_file(store_root, actor_id_hex) else {
            return false;
        };
        matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock))
    }
}

/// Result of [`AgentPresenceLock::acquire`].
///
/// There is deliberately **no `Refused`**: the acquire blocks, and the only
/// other takers are the apps' momentary probes, so a live holder is a wait of
/// one probe's length, not an outcome.
#[derive(Debug)]
pub enum AgentPresenceLockOutcome {
    /// The agent is now visibly hosting the store; the lock releases when the
    /// value drops (or the process dies).
    Held(AgentPresenceLock),
    /// The lock could not be taken. The agent mounts anyway (`account-runtime.md`
    /// § Multi-instance concurrency → *The agent holds the role when present*,
    /// part 1): this lock states priority, `engine.lock` keeps exclusivity, and
    /// the apps then see the first-come election.
    Degraded(std::io::Error),
}

/// What [`AgentPresenceLock::is_present`] found.
#[derive(Debug)]
pub enum AgentPresence {
    /// The agent holds the lock — it hosts this store.
    Present,
    /// Nobody holds it.
    Absent,
    /// The probe could not ask. The caller owns what that means: a seed-holding
    /// runtime reads it as `Absent` before a try (degrade open — a lone app
    /// must pump) and keeps the role it holds (a degrade is never a yield).
    Degraded(std::io::Error),
}

/// Held agent presence lock; RAII — dropping releases.
#[derive(Debug)]
pub struct AgentPresenceLock {
    /// Held (and thereby exclusively locked) until drop; never read.
    _file: File,
}

impl AgentPresenceLock {
    /// Declare that the sync agent hosts the store rooted at `store_dir`, for
    /// as long as the returned lock lives. The agent's mount takes it before
    /// its engine election; a seedless runtime never takes it itself — its
    /// host does.
    ///
    /// An **exclusive blocking** acquire, and the wait is bounded by
    /// construction: the agent is single-instance per user, so no second
    /// exclusive taker exists, and every other taker is [`Self::is_present`],
    /// which holds a shared lock for the length of one `try_lock_shared`. A
    /// `try` here would instead read a probe's instant as "another agent" —
    /// the race the blocking acquire exists to close ([`ServingLock::acquire`]
    /// with the sides swapped).
    pub fn acquire(store_dir: &Path) -> AgentPresenceLockOutcome {
        let file = match open_lock_file(store_dir, &agent_lock_path(store_dir)) {
            Ok(f) => f,
            Err(e) => return AgentPresenceLockOutcome::Degraded(e),
        };
        match file.lock() {
            Ok(()) => AgentPresenceLockOutcome::Held(Self { _file: file }),
            Err(e) => AgentPresenceLockOutcome::Degraded(e),
        }
    }

    /// **Does the sync agent host the store rooted at `store_dir`?** — the
    /// question a seed-holding runtime asks before every engine try and while
    /// it holds the role.
    ///
    /// A shared try, dropped at once: winning it proves no exclusive holder,
    /// and holding nothing afterwards restores what the probe found.
    pub fn is_present(store_dir: &Path) -> AgentPresence {
        let file = match open_lock_file(store_dir, &agent_lock_path(store_dir)) {
            Ok(f) => f,
            Err(e) => return AgentPresence::Degraded(e),
        };
        match file.try_lock_shared() {
            Ok(()) => AgentPresence::Absent,
            Err(std::fs::TryLockError::WouldBlock) => AgentPresence::Present,
            Err(std::fs::TryLockError::Error(e)) => AgentPresence::Degraded(e),
        }
    }
}

fn open_serving_lock_file(store_root: &Path, actor_id_hex: &str) -> anyhow::Result<File> {
    let path = serving_lock_path(store_root, actor_id_hex)?;
    Ok(fauna_core::fs_lock::open_lock_file(&path)?)
}

/// Open (creating if absent) one of the reserved zero-byte lock files, with
/// the store dir created if it does not exist yet — both locks may be taken
/// before anything else has touched the dir.
///
/// The mint is the workspace-shared [`fauna_core::fs_lock`] (never-truncate,
/// never-delete, owner-only — lifted 2026-08-15 when the MLS role lock became
/// its third consumer); it creates the lock file's parent, which for these
/// reserved store-dir siblings is exactly `store_dir`.
fn open_lock_file(store_dir: &Path, lock_path: &Path) -> std::io::Result<File> {
    debug_assert_eq!(lock_path.parent(), Some(store_dir));
    fauna_core::fs_lock::open_lock_file(lock_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(outcome: EngineLockOutcome) -> EngineLock {
        match outcome {
            EngineLockOutcome::Held(lock) => lock,
            other => panic!("expected Held, got {other:?}"),
        }
    }

    fn held_migration(outcome: MigrationLockOutcome) -> MigrationLock {
        match outcome {
            MigrationLockOutcome::Held(lock) => lock,
            other => panic!("expected Held, got {other:?}"),
        }
    }

    /// The exclusion pin (the `sync-agent.md` § single-instance test shape):
    /// a second acquire is refused while the first holds, and succeeds once
    /// the holder releases. Two same-process opens contend because the lock
    /// lives on the open file description.
    #[test]
    fn a_second_acquire_is_refused_while_held_and_succeeds_after_release() {
        let tmp = tempfile::tempdir().unwrap();
        let store_dir = tmp.path().join("store");

        let first = held(EngineLock::try_acquire(&store_dir));
        assert!(
            matches!(
                EngineLock::try_acquire(&store_dir),
                EngineLockOutcome::Refused
            ),
            "a live holder must refuse a second acquire"
        );
        drop(first);
        let _reacquired = held(EngineLock::try_acquire(&store_dir));
    }

    /// The election contends on the reserved name beside the store DB —
    /// `engine.lock`, the file `store.rs` reserves — and never deletes it:
    /// release leaves the zero-byte file for the next acquirer to lock.
    #[test]
    fn the_lock_is_the_reserved_file_and_release_never_deletes_it() {
        let tmp = tempfile::tempdir().unwrap();
        let store_dir = tmp.path().join("store");

        let lock = held(EngineLock::try_acquire(&store_dir));
        let lock_path = engine_lock_path(&store_dir);
        assert!(lock_path.ends_with("engine.lock"));
        assert!(lock_path.exists(), "acquire mints the lock file");
        drop(lock);
        assert!(
            lock_path.exists(),
            "release must not unlink the lock file (unlinking re-opens the race)"
        );
    }

    /// Acquisition on a store dir that does not exist yet creates it — the
    /// runtime may elect before or after the store's own open.
    #[test]
    fn acquire_creates_a_missing_store_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let store_dir = tmp.path().join("not-yet").join("store");
        let _lock = held(EngineLock::try_acquire(&store_dir));
        assert!(store_dir.is_dir());
    }

    /// The two locks are **independent**: holding the role must not make a
    /// cold opener wait for migration, and migrating must not cost a process
    /// the role. Distinct files is the whole mechanism, so this pins that
    /// they are distinct.
    #[test]
    fn the_engine_role_and_the_migration_section_do_not_contend() {
        let tmp = tempfile::tempdir().unwrap();
        let store_dir = tmp.path().join("store");

        let _role = held(EngineLock::try_acquire(&store_dir));
        let _section = held_migration(MigrationLock::acquire(&store_dir));
        assert_ne!(
            engine_lock_path(&store_dir),
            migration_lock_path(&store_dir),
            "one file for both sections would serialize a cold open behind the engine role"
        );
    }

    fn held_seed_legs(outcome: SeedLegLockOutcome) -> SeedLegLock {
        match outcome {
            SeedLegLockOutcome::Held(lock) => lock,
            other => panic!("expected Held, got {other:?}"),
        }
    }

    /// The seed-leg role's exclusion pin, the engine role's own shape: a
    /// second acquire is refused while the first holds, and succeeds once the
    /// holder releases.
    #[test]
    fn a_second_seed_leg_acquire_is_refused_while_held_and_succeeds_after_release() {
        let tmp = tempfile::tempdir().unwrap();
        let store_dir = tmp.path().join("store");

        let first = held_seed_legs(SeedLegLock::try_acquire(&store_dir));
        assert!(
            matches!(
                SeedLegLock::try_acquire(&store_dir),
                SeedLegLockOutcome::Refused
            ),
            "a live holder must refuse a second acquire"
        );
        drop(first);
        let _reacquired = held_seed_legs(SeedLegLock::try_acquire(&store_dir));
    }

    /// The seed-leg role contends on its own reserved name — `seed-legs.lock`
    /// — creates a missing store dir, and never deletes the file.
    #[test]
    fn the_seed_leg_lock_is_the_reserved_file_and_release_never_deletes_it() {
        let tmp = tempfile::tempdir().unwrap();
        let store_dir = tmp.path().join("not-yet").join("store");

        let lock = held_seed_legs(SeedLegLock::try_acquire(&store_dir));
        let lock_path = seed_legs_lock_path(&store_dir);
        assert!(lock_path.ends_with("seed-legs.lock"));
        assert!(lock_path.exists(), "acquire mints the lock file");
        drop(lock);
        assert!(
            lock_path.exists(),
            "release must not unlink the lock file (unlinking re-opens the race)"
        );
    }

    /// The two roles are **independent**: the process that pumps and the
    /// process that runs the seed-only legs are different processes in the
    /// steady state the seed-leg role exists for (a seedless agent beside a
    /// signed-in app), so holding either must never refuse the other — and
    /// neither waits on, nor is refused by, a migration section.
    #[test]
    fn the_engine_role_and_the_seed_leg_role_do_not_contend() {
        let tmp = tempfile::tempdir().unwrap();
        let store_dir = tmp.path().join("store");

        let _engine = held(EngineLock::try_acquire(&store_dir));
        let _seed_legs = held_seed_legs(SeedLegLock::try_acquire(&store_dir));
        let _section = held_migration(MigrationLock::acquire(&store_dir));
        assert!(
            matches!(
                EngineLock::try_acquire(&store_dir),
                EngineLockOutcome::Refused
            ),
            "each role still excludes a second holder of itself"
        );
        assert_ne!(
            engine_lock_path(&store_dir),
            seed_legs_lock_path(&store_dir)
        );
    }

    /// The migration lock **serializes rather than refuses** — the
    /// blocking-vs-try distinction that makes it a critical section and not
    /// an election.
    ///
    /// Causal, not timed (convention 14): the second acquire runs on a
    /// thread that signals *before* it blocks, the test waits for that
    /// signal, drops the holder, and then joins. A pass therefore proves the
    /// waiter got the lock **after** the holder released it — the join is
    /// the only synchronization, and there is no sleep or deadline anywhere.
    #[test]
    fn the_migration_lock_blocks_the_second_acquirer_until_the_first_releases() {
        let tmp = tempfile::tempdir().unwrap();
        let store_dir = tmp.path().join("store");

        let first = held_migration(MigrationLock::acquire(&store_dir));
        let (about_to_block_tx, about_to_block_rx) = std::sync::mpsc::channel();
        let waiter_dir = store_dir.clone();
        let waiter = std::thread::spawn(move || {
            about_to_block_tx.send(()).unwrap();
            held_migration(MigrationLock::acquire(&waiter_dir))
        });

        about_to_block_rx.recv().unwrap();
        // The waiter cannot have the lock: this thread still holds it, and a
        // lock is exclusive. Releasing is what lets the join below finish —
        // a `try_lock` would have made the thread return immediately with a
        // refusal instead, so a hang here is the failure signal for a
        // regression to non-blocking.
        drop(first);
        let _second = waiter.join().expect("the waiter acquires after release");
    }

    /// The migration lock uses its own reserved name and, like the election
    /// lock, never unlinks it.
    #[test]
    fn the_migration_lock_is_the_reserved_file_and_release_never_deletes_it() {
        let tmp = tempfile::tempdir().unwrap();
        let store_dir = tmp.path().join("store");

        let lock = held_migration(MigrationLock::acquire(&store_dir));
        let lock_path = migration_lock_path(&store_dir);
        assert!(lock_path.ends_with("migration.lock"));
        assert!(lock_path.exists(), "acquire mints the lock file");
        drop(lock);
        assert!(
            lock_path.exists(),
            "release must not unlink the lock file (unlinking re-opens the race)"
        );
    }

    const ACTOR: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";

    fn serving(outcome: ServingLockOutcome) -> ServingLock {
        match outcome {
            ServingLockOutcome::Held(lock) => lock,
            other => panic!("expected Held, got {other:?}"),
        }
    }

    /// The whole point: a serving instance is visible to an erase's probe for
    /// exactly as long as it serves.
    #[test]
    fn a_serving_instance_is_seen_until_it_stops_serving() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            !ServingLock::is_served(root.path(), ACTOR),
            "nobody serves yet"
        );

        let instance = serving(ServingLock::acquire(root.path(), ACTOR));
        assert!(ServingLock::is_served(root.path(), ACTOR));

        drop(instance);
        assert!(
            !ServingLock::is_served(root.path(), ACTOR),
            "the probe must leave nothing held behind it either"
        );
    }

    /// Presence, not exclusion: two apps serving one account is the supported
    /// case, and the lock must never be what refuses it.
    #[test]
    fn two_instances_serve_one_account_side_by_side() {
        let root = tempfile::tempdir().unwrap();
        let _tui = serving(ServingLock::acquire(root.path(), ACTOR));
        let linux = serving(ServingLock::acquire(root.path(), ACTOR));
        drop(linux);
        assert!(
            ServingLock::is_served(root.path(), ACTOR),
            "one of two leaving must not make the account look free"
        );
    }

    /// ⚠ The placement pin. Erasing the account — per-actor or the whole-root
    /// sweep — must leave the lock file where it is: a guard the erase unlinks
    /// is a guard the next instance locks a *different inode* of.
    #[test]
    fn the_serving_lock_file_sits_at_the_root_and_survives_both_erases() {
        let root = tempfile::tempdir().unwrap();
        let lock = serving(ServingLock::acquire(root.path(), ACTOR));
        let lock_path = serving_lock_path(root.path(), ACTOR).unwrap();
        assert_eq!(lock_path.parent(), Some(root.path()));
        drop(lock);

        std::fs::create_dir_all(root.path().join(ACTOR).join("account-store")).unwrap();
        assert!(crate::db::erase_actor_state([root.path()], ACTOR).is_empty());
        assert!(lock_path.exists(), "the per-actor erase unlinked the guard");

        std::fs::create_dir_all(root.path().join(ACTOR)).unwrap();
        assert!(crate::db::erase_all_account_scopes(root.path()).is_clean());
        assert!(
            lock_path.exists(),
            "the whole-root sweep unlinked the guard"
        );
    }

    fn agent(outcome: AgentPresenceLockOutcome) -> AgentPresenceLock {
        match outcome {
            AgentPresenceLockOutcome::Held(lock) => lock,
            other => panic!("expected Held, got {other:?}"),
        }
    }

    /// The presence pin: an agent holding the lock is seen by the probe, a
    /// dropped one is not — and the probe itself leaves nothing held behind,
    /// or it would read as an agent to the next probe.
    #[test]
    fn an_agent_holder_is_seen_until_it_drops() {
        let tmp = tempfile::tempdir().unwrap();
        let store_dir = tmp.path().join("store");

        assert!(matches!(
            AgentPresenceLock::is_present(&store_dir),
            AgentPresence::Absent
        ));
        assert!(
            matches!(
                AgentPresenceLock::is_present(&store_dir),
                AgentPresence::Absent
            ),
            "a probe must release what it took"
        );
        let held = agent(AgentPresenceLock::acquire(&store_dir));
        assert!(
            matches!(
                AgentPresenceLock::is_present(&store_dir),
                AgentPresence::Present
            ),
            "an exclusive holder refuses the probe's shared try"
        );
        drop(held);
        assert!(
            matches!(
                AgentPresenceLock::is_present(&store_dir),
                AgentPresence::Absent
            ),
            "a dropped holder admits the probe"
        );
    }

    /// The acquire **blocks** behind a probe rather than refusing — causal, the
    /// migration pin's shape: a shared holder stands in for a probe caught
    /// mid-flight, the acquiring thread signals before it blocks, and the join
    /// can finish only once the shared holder is gone.
    #[test]
    fn the_agent_acquire_waits_out_a_probe_instead_of_refusing() {
        let tmp = tempfile::tempdir().unwrap();
        let store_dir = tmp.path().join("store");

        let probe = fauna_core::fs_lock::open_lock_file(&agent_lock_path(&store_dir)).unwrap();
        probe.try_lock_shared().unwrap();
        let (about_to_block_tx, about_to_block_rx) = std::sync::mpsc::channel();
        let waiter_dir = store_dir.clone();
        let waiter = std::thread::spawn(move || {
            about_to_block_tx.send(()).unwrap();
            agent(AgentPresenceLock::acquire(&waiter_dir))
        });
        about_to_block_rx.recv().unwrap();
        drop(probe);
        let _held = waiter.join().expect("the agent acquires after the probe");
    }

    /// The reserved name, independent of every other lock, never deleted. The
    /// agent holds presence beside the engine role — or beside an app's — so
    /// neither may refuse the other.
    #[test]
    fn the_agent_lock_is_the_reserved_file_independent_and_never_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let store_dir = tmp.path().join("not-yet").join("store");

        let presence = agent(AgentPresenceLock::acquire(&store_dir));
        let lock_path = agent_lock_path(&store_dir);
        assert!(lock_path.ends_with("agent.lock"));
        assert!(lock_path.exists(), "acquire mints the lock file");
        let _engine = held(EngineLock::try_acquire(&store_dir));
        let _seed_legs = held_seed_legs(SeedLegLock::try_acquire(&store_dir));
        drop(presence);
        assert!(
            lock_path.exists(),
            "release must not unlink the lock file (unlinking re-opens the race)"
        );
    }

    /// Junk hex mints nothing, and an unreachable root reports free.
    #[test]
    fn a_malformed_actor_or_an_unreachable_root_degrades_open() {
        let root = tempfile::tempdir().unwrap();
        assert!(serving_lock_path(root.path(), "not-hex").is_err());
        assert!(!ServingLock::is_served(root.path(), "not-hex"));
        assert!(matches!(
            ServingLock::acquire(root.path(), "not-hex"),
            ServingLockOutcome::Degraded(_)
        ));

        let plain_file = root.path().join("a-file");
        std::fs::write(&plain_file, b"").unwrap();
        assert!(!ServingLock::is_served(&plain_file, ACTOR));
    }
}
