//! Cross-process advisory mutation lock for the account registry.
//!
//! Concurrent instances make registry mutation genuinely multi-process
//! (`docs/goal/architecture/apps/account-scoping.md` § Concurrent
//! instances): every mutator is a read-modify-write over the single
//! `fauna/index` blob (plus multi-key slot sequences), and no
//! [`SecretStore`](crate::SecretStore) backend offers cross-process
//! transactions — so two instances mutating concurrently can lose an
//! update (one rewrite of the index swallowing the other's) or interleave
//! an erase with a migration. The lock serializes those windows.
//!
//! **Advisory, and sufficient**: every mutation path on every platform
//! flows through this one crate (`long-term-store.md` § Multi-account
//! evolution → Shared seam), so cooperating through an advisory lock covers
//! all writers — there is no second implementation to forget it. That holds
//! only because nothing downstream can mint an *unlocked* registry behind the
//! platform's back: the `LaunchPersistence` adapter is built from a registry
//! ([`AccountRegistry::launch_persistence`](crate::AccountRegistry::launch_persistence))
//! and has no store-taking constructor, so the launch writer —
//! `save_authenticated`, a full index read-modify-write — inherits whatever
//! lock the client chose. Keep it that way.
//!
//! **Reads never lock.** The registry's "a read never writes" invariant
//! extends here: reads and the [`bind_account`](crate::AccountRegistry::bind_account)
//! spawn gate stay wait-free, so a wedged holder can never block another
//! instance's launch. The OS releases the file lock when its holder dies,
//! so a crashed mutator cannot orphan it.
//!
//! **Degrades open.** A lock-file I/O failure yields an unheld guard —
//! exactly the pre-lock behavior — rather than turning a filesystem hiccup
//! into a broken sign-out or switch. The lock narrows a race; it must not
//! widen a failure.

use std::any::Any;

/// Serializes registry mutations across processes (and threads) of one
/// install. Implementations must be safe to call from any thread; `acquire`
/// blocks until the lock is held.
pub trait MutationLock: Send + Sync {
    /// Block until the mutation lock is held. The returned guard releases on
    /// drop. On an I/O failure the guard is unheld (advisory degrade — see
    /// module docs).
    fn acquire(&self) -> MutationLockGuard;
}

/// Opaque RAII guard for [`MutationLock::acquire`]; releases on drop.
pub struct MutationLockGuard {
    /// Whatever OS resource holds the lock (`std::fs::File` for
    /// [`FileMutationLock`]); dropping it releases. `None` = unheld (noop or
    /// degraded acquire).
    _held: Option<Box<dyn Any + Send>>,
}

impl MutationLockGuard {
    /// A guard holding nothing (noop lock, or a degraded acquire).
    pub fn unheld() -> Self {
        Self { _held: None }
    }

    /// A guard whose dropped resource releases the lock.
    pub fn holding(resource: Box<dyn Any + Send>) -> Self {
        Self {
            _held: Some(resource),
        }
    }
}

/// The no-op lock: wasm — whose cross-tab leg is
/// `web_mutation_lock::with_web_mutation_lock`, taken by every wasm-facing
/// mutator entry from OUTSIDE the synchronous mutator because a Web Lock is
/// async (the `web-localstorage` feature) — and every pre-lock constructor
/// path.
pub struct NoopMutationLock;

impl MutationLock for NoopMutationLock {
    fn acquire(&self) -> MutationLockGuard {
        MutationLockGuard::unheld()
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use file_lock::FileMutationLock;

#[cfg(not(target_arch = "wasm32"))]
mod file_lock {
    use std::fs::File;
    use std::path::{Path, PathBuf};

    use super::{MutationLock, MutationLockGuard};

    /// Name of the lock file inside the install-scoped state directory.
    const LOCK_FILE_NAME: &str = "account-registry.lock";

    /// OS-file-lock implementation over std's stabilized `File::lock` (the
    /// same kernel-arbitrated mechanism as
    /// `fauna_ipc::unix_transport::InstanceLock` — `flock` on unix,
    /// `LockFileEx` on windows; no third-party dependency).
    ///
    /// The path must be **install-scoped** (one per OS login's state base,
    /// e.g. `~/Library/Application Support/Fauna/`), never per-account: the
    /// lock protects the shared `fauna/index`.
    pub struct FileMutationLock {
        lock_path: PathBuf,
    }

    impl FileMutationLock {
        /// Lock over `<state_dir>/account-registry.lock`. The directory is
        /// created on first acquire if missing.
        pub fn new(state_dir: &Path) -> Self {
            Self {
                lock_path: state_dir.join(LOCK_FILE_NAME),
            }
        }

        fn open_and_lock(&self) -> std::io::Result<File> {
            let file = crate::lock_file::open_lock_file(&self.lock_path)?;
            file.lock()?;
            Ok(file)
        }
    }

    impl MutationLock for FileMutationLock {
        fn acquire(&self) -> MutationLockGuard {
            match self.open_and_lock() {
                // Dropping the `File` releases the lock (and the OS releases
                // it if the holder dies — no orphaned lock is possible).
                Ok(file) => MutationLockGuard::holding(Box::new(file)),
                // Advisory degrade: an unlockable path yields today's
                // unserialized behavior rather than a broken sign-out/switch
                // (module docs — the lock narrows a race, never widens a
                // failure).
                Err(_) => MutationLockGuard::unheld(),
            }
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use super::*;

    /// Two `FileMutationLock`s over one directory exclude each other: the
    /// second `acquire` does not return while the first guard lives.
    #[test]
    fn a_second_holder_is_excluded_until_the_first_releases() {
        let dir = tempfile::tempdir().unwrap();
        let lock_a = FileMutationLock::new(dir.path());
        let lock_b = FileMutationLock::new(dir.path());

        let guard_a = lock_a.acquire();
        let (started_tx, started_rx) = mpsc::channel();
        let (acquired_tx, acquired_rx) = mpsc::channel();
        let b = thread::spawn(move || {
            started_tx.send(()).unwrap();
            let _guard_b = lock_b.acquire();
            acquired_tx.send(()).unwrap();
        });

        started_rx.recv().unwrap();
        // Bounded negative check: B must still be blocked while A holds.
        assert!(
            acquired_rx
                .recv_timeout(Duration::from_millis(200))
                .is_err(),
            "second holder acquired while the first guard was still live"
        );
        drop(guard_a);
        acquired_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("second holder must acquire once the first releases");
        b.join().unwrap();
    }

    /// An impossible lock path (parent is a regular file) degrades to an
    /// unheld guard instead of panicking or erroring the mutation path.
    #[test]
    fn an_impossible_lock_path_yields_an_unheld_guard() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("plain-file");
        std::fs::write(&plain, b"x").unwrap();
        let lock = FileMutationLock::new(&plain.join("sub"));
        let _guard = lock.acquire(); // must not panic; drop must be a no-op
    }
}
