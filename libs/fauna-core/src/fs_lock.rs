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
///
/// **Owner-only from the first instant, and never through a symlink (unix).**
/// Holding a lock file's descriptor is an availability lever since the agent's
/// presence lock (`fauna_account_store::locks::AgentPresenceLock`): another OS
/// user who opens the file can keep a shared lock the agent's acquire then
/// waits on. So the file is CREATED with mode `0600` — no umask window between
/// the create and a chmod; a path that is a symlink, or that no longer names
/// the inode this open got, is refused before anything is changed through it;
/// and a pre-existing file is tightened to `0600` through the descriptor
/// (`fchmod`), a failure refusing the open rather than being ignored. The
/// symlink refusal is an identity check after the open rather than
/// `O_NOFOLLOW`: this crate links no `libc`, and the lock files live in
/// owner-only directories, where a swap between the open and the check is no
/// other user's to make.
pub fn open_lock_file(path: &Path) -> std::io::Result<File> {
    open_lock_file_observed(path, |_| {})
}

/// [`open_lock_file`] with `after_open` called on the descriptor straight
/// after the open, before the identity check and the `fchmod` — the one point
/// at which the create mode alone decides the file's permissions. Production
/// passes a no-op; the tests read the mode there, since afterwards the
/// `fchmod` would mask a dropped create mode.
fn open_lock_file_observed(path: &Path, after_open: impl FnOnce(&File)) -> std::io::Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.create(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    after_open(&file);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let at_path = std::fs::symlink_metadata(path)?;
        let opened = file.metadata()?;
        if at_path.file_type().is_symlink()
            || at_path.dev() != opened.dev()
            || at_path.ino() != opened.ino()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "lock file {} is a symlink or was replaced during the open",
                    path.display()
                ),
            ));
        }
        if opened.permissions().mode() & 0o777 != 0o600 {
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
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

    /// Owner-only from the open itself: under a umask that would leave the
    /// file world-readable and -writable, the mode read straight after the
    /// open is already `0600`. The umask is process-wide, so the open runs in
    /// a child of this test binary (`FAUNA_FS_LOCK_UMASK_CHILD`) started under
    /// `umask 000`, never under the other tests' threads.
    #[cfg(unix)]
    #[test]
    fn creates_the_file_owner_only_under_a_permissive_umask() {
        use std::os::unix::fs::PermissionsExt;
        if let Some(path) = std::env::var_os("FAUNA_FS_LOCK_UMASK_CHILD") {
            let _file = open_lock_file(Path::new(&path)).unwrap();
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "owner-only straight after the open");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("umask.lock");
        let status = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(r#"umask 000 && exec "$0" --exact "$1""#)
            .arg(std::env::current_exe().unwrap())
            .arg("fs_lock::tests::creates_the_file_owner_only_under_a_permissive_umask")
            .env("FAUNA_FS_LOCK_UMASK_CHILD", &path)
            .status()
            .unwrap();
        assert!(status.success(), "the child's open under umask 000");
        assert!(path.exists(), "the child created the file");
    }

    /// A pre-existing file left permissive is tightened by the open, and a
    /// symlink at the lock path is refused without touching its target.
    #[cfg(unix)]
    #[test]
    fn tightens_an_existing_file_and_refuses_a_symlink() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("old.lock");
        std::fs::write(&path, b"").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        let _file = open_lock_file(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "an existing lock file is tightened");

        let target = tmp.path().join("target");
        std::fs::write(&target, b"").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        let link = tmp.path().join("link.lock");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(
            open_lock_file(&link).is_err(),
            "a symlinked lock path is refused"
        );
        let mode = std::fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o644,
            "nothing is changed through the symlink"
        );
    }

    /// The create mode alone makes the file owner-only: the mode is read off the
    /// descriptor straight after the open, before the `fchmod` could tighten
    /// it, so dropping `options.mode(0o600)` while the `fchmod` stays reddens
    /// this and nothing else. Same umask-000 child as the pin above.
    #[cfg(unix)]
    #[test]
    fn the_create_mode_alone_is_owner_only_before_any_chmod() {
        use std::os::unix::fs::PermissionsExt;
        if let Some(path) = std::env::var_os("FAUNA_FS_LOCK_CREATE_MODE_CHILD") {
            let mut seen = None;
            let _file = open_lock_file_observed(Path::new(&path), |f| {
                seen = Some(f.metadata().unwrap().permissions().mode());
            })
            .unwrap();
            assert_eq!(
                seen.expect("the hook ran") & 0o777,
                0o600,
                "owner-only at the create, before any chmod"
            );
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("create-mode.lock");
        let status = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(r#"umask 000 && exec "$0" --exact "$1""#)
            .arg(std::env::current_exe().unwrap())
            .arg("fs_lock::tests::the_create_mode_alone_is_owner_only_before_any_chmod")
            .env("FAUNA_FS_LOCK_CREATE_MODE_CHILD", &path)
            .status()
            .unwrap();
        assert!(status.success(), "the child's open under umask 000");
        assert!(path.exists(), "the child created the file");
    }
}
