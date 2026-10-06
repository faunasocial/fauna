//! Shared prelude for the workspace's kernel-arbitrated advisory file locks.
//!
//! Every file lock in the tree uses the same mechanism — `flock` on unix,
//! `LockFileEx` on windows, via std's `File::lock`/`try_lock` family — over a
//! zero-byte `0600` file whose *name*, never its content, is the key. The
//! consumers each own their lock's semantics (try vs blocking, refuse vs
//! degrade); what they share is the mint below and three invariants its doc
//! comment carries so no copy drifts:
//!
//! - **Never truncate.** Truncating a file another process holds locked is a
//!   write to shared state for no reason.
//! - **Never delete a lock file.** Unlinking re-opens the race it closes: a
//!   new acquirer can lock the orphaned inode while another process creates a
//!   fresh file at the same path.
//! - **Crash-safe by construction.** The kernel releases the lock when its
//!   holder dies, so there is no stale-lock reconciliation anywhere.
//!
//! Consumers: `fauna_client_accounts` (the account instance + mutation
//! locks), `fauna_account_store::locks` (the engine-singleton election + the
//! migration critical section), and `fauna_mls::storage` (the
//! conversations-engine role lock).
//!
//! **One lock in the tree deliberately does NOT call this** and never will
//! without a new reason (ruled 2026-08-15 — do not re-flag it as an
//! oversight): the sync agent's socket `InstanceLock` in
//! `fauna_ipc::unix_transport`. `fauna-ipc` has no dependency on this crate,
//! and adding one to share six lines of `std` would pull this crate's entire
//! cryptographic graph into the deliberately lean Windows shell extension that
//! links `fauna-ipc`. Its copy is held to the same three invariants by a test
//! rather than by a shared symbol, which is the cheaper guarantee here. The
//! full reasoning, and the trigger that would reverse it — a *second*
//! `fauna-core`-free crate needing the mint, at which point a leaf crate
//! holding just this function starts paying for itself — lives on
//! `InstanceLock`'s own doc comment.

use std::fs::{File, OpenOptions};
use std::path::Path;

/// Open (creating if absent) the lock file at `path`, creating its parent
/// directory if missing and restricting the file to owner-only on unix.
/// Callers take the OS-level lock themselves — non-blocking `try_lock()` /
/// `try_lock_shared()` or blocking `lock()`, whichever their contract needs.
pub fn open_lock_file(path: &Path) -> std::io::Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)?;
    // A zero-content lock file, not secret-bearing — the umask-window class a
    // write-then-chmod secret write must worry about does not apply here.
    // Reviewed and cleared, no rewrite needed.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mints_a_lockable_owner_only_file_and_never_truncates() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("deep").join("test.lock");

        let file = open_lock_file(&path).unwrap();
        file.try_lock().unwrap();
        assert!(path.exists(), "the mint creates the file (and its parent)");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "owner-only on unix");
        }
        drop(file);

        // A second open of an existing (now non-empty) file must not truncate:
        // write a byte, re-open, and the byte survives.
        std::fs::write(&path, b"x").unwrap();
        let _again = open_lock_file(&path).unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"x",
            "re-opening a lock file must never truncate it"
        );
    }
}
