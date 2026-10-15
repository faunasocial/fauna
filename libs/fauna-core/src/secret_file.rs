//! Shared mint for an owner-only (`0o600`) secret file: a boot-minted
//! credential, key seed, or token that must never be readable by another
//! local principal even for the brief window between file creation and a
//! trailing `chmod` (BR-4 defense-in-depth).
//!
//! Lifted from `bins/fauna-nest/src/deployment_key.rs::write_secret_file_0600`
//! (the original per-crate copy) when a second binary,
//! `bins/fauna-iroh-relay`, needed the identical shape for its relay X25519
//! key — sharing the tested
//! implementation instead of letting a second copy drift the way
//! `write_secret_file_0600` itself once did: a rewrite dropped the `set_permissions` on the false premise
//! that its `.tmp` temp file is always fresh.
//!
//! Consumers: `fauna-nest` (the deployment-key seed, sidecar tokens, the
//! web-serve holder seed), `fauna-iroh-relay` (the persisted X25519 relay
//! keypair).

use std::path::Path;
use std::path::PathBuf;

use anyhow::Result;

/// As [`write_secret_file_0600`], but stages the write at the caller-supplied
/// `tmp_path` instead of the default `<path>.tmp` — for a caller that needs
/// more than one unlocked writer of the same `path`'s directory live at
/// once, each disambiguated by its own temp-name scheme (pid, sequence, …)
/// rather than sharing the single default sibling (e.g.
/// `fauna_credential_store::cred_file_write`, whose namespace file a test
/// seeder may write outside the normal per-namespace lock).
///
/// **Crash-atomic: write the temp file, `fsync` it, `rename(2)` over
/// `path`, `fsync` the parent directory.** A crash at any point before the
/// rename leaves `path` untouched (the old bytes, or absent, if this is the
/// first write); a crash after leaves it wholly the new bytes. A prior
/// truncate-then-write directly on `path` could leave `path` zero-length or
/// short if a crash landed between the truncate and `write_all`, which
/// matters for any caller whose file a client TOFU-pins or otherwise cannot
/// recover from a short read on next boot.
///
/// **On unix, the temp open is made FRESH BY CONSTRUCTION, not assumed.**
/// `.mode(0o600)` binds only at CREATION — a crash between this function's
/// own open and its rename below leaves `tmp_path` on disk, and reopening
/// that same path with a plain `create(true)` would silently inherit
/// whatever mode (or symlink target) was already there, defeating the
/// `0600` guarantee for exactly the crash case this function exists to
/// reason about. So the stale sibling is unlinked first, then the open uses
/// `create_new` (`O_CREAT|O_EXCL`), which refuses outright if anything —
/// file or symlink — already sits at `tmp_path`. Between those two steps
/// there is no window in which this open could bind to something it did not
/// just create, so `.mode()` alone is sufficient and no trailing
/// `set_permissions` is needed — this is the reference-implementation shape
/// for a target that CAN pre-exist: unlink-then-`O_EXCL`, not a
/// narrow-after-the-fact `chmod`. On other targets the file inherits the
/// default perms — every production consumer of the `0600` guarantee itself
/// is Linux today — but the temp-then-rename atomicity still applies.
pub fn write_secret_file_0600_at(path: &Path, tmp_path: &Path, bytes: &[u8]) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;

        // Unlink any crash residue before the exclusive create — see the
        // doc comment above. `remove_file` on a symlink removes the LINK
        // only, never its target, so this cannot touch anything outside
        // this one `tmp_path`.
        if let Err(e) = std::fs::remove_file(tmp_path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            return Err(e.into());
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(tmp_path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
    }
    #[cfg(not(unix))]
    std::fs::write(tmp_path, bytes)?;

    if let Err(e) = replace_file(tmp_path, path) {
        let _ = std::fs::remove_file(tmp_path);
        return Err(e.into());
    }

    // Durability of the rename itself: POSIX rename is atomic for
    // VISIBILITY (no reader ever sees a half-renamed name), but the
    // directory entry update still needs its own fsync to survive a
    // crash right after the rename.
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

/// How long [`replace_file`] rides out a windows reader holding `path` open.
/// A reader of these files holds its handle for one read, so this is far
/// above any real hold; it is the bound on a pathological one, not a wait a
/// healthy write ever pays.
#[cfg(windows)]
const REPLACE_RETRY_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);

/// `rename(tmp_path, path)`, riding out a windows reader that holds `path`
/// open without `FILE_SHARE_DELETE`.
///
/// On unix a rename replaces a file other processes hold open. On windows it
/// cannot while any handle on the target refused delete sharing, and the
/// commonest such handle is a plain read: a C runtime `open()` — every
/// CPython `open()`, so every e2e harness read of a credential file — shares
/// read and write but never delete. The rename then fails with a sharing
/// violation (or access denied) for exactly as long as that read is open, and
/// a caller that gives up loses the write: measured 2026-10-10 on the windows
/// app's credential file while a harness poll read it. So on windows a
/// transient refusal is retried, backing off, for at most
/// [`REPLACE_RETRY_BUDGET`]; any other error, or one that outlives the
/// budget, is returned as before.
fn replace_file(tmp_path: &Path, path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        // ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION.
        const TRANSIENT: [i32; 3] = [5, 32, 33];
        let deadline = std::time::Instant::now() + REPLACE_RETRY_BUDGET;
        let mut backoff = std::time::Duration::from_millis(5);
        loop {
            match std::fs::rename(tmp_path, path) {
                Err(e)
                    if e.raw_os_error()
                        .is_some_and(|code| TRANSIENT.contains(&code))
                        && std::time::Instant::now() < deadline =>
                {
                    #[cfg(test)]
                    tests::on_replace_retry();
                    std::thread::sleep(backoff);
                    backoff = (backoff * 2).min(std::time::Duration::from_millis(100));
                }
                other => return other,
            }
        }
    }
    #[cfg(not(windows))]
    std::fs::rename(tmp_path, path)
}

/// Write `bytes` to `path` as an owner-only (`0o600`) file, staging the
/// write at the default `<path>.tmp` sibling (same directory as `path`, so
/// the rename is same-filesystem and therefore atomic). No other principal
/// on the box should read the file. See [`write_secret_file_0600_at`] for
/// the full crash-safety argument.
pub fn write_secret_file_0600(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut tmp_name = path.as_os_str().to_os_string();
    tmp_name.push(".tmp");
    write_secret_file_0600_at(path, &PathBuf::from(tmp_name), bytes)
}

#[cfg(test)]
mod tests {
    // Most tests below have a `#[cfg(unix)]` body for mode/inode/symlink
    // properties that only exist on the unix arm; `distinct_tmp_paths_*`
    // below runs on every platform and is what keeps this import used on
    // windows too (a `-D warnings` unused-import failure otherwise).
    use super::*;

    // What [`replace_file`]'s windows retry runs on each transient refusal —
    // the test's handle on the moment the rename was refused, so a reader can
    // be released exactly then rather than after a guessed sleep. Per thread:
    // the retry runs on the writing thread, and parallel tests never see each
    // other's hook.
    #[cfg(windows)]
    thread_local! {
        static RETRY_HOOK: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
            const { std::cell::RefCell::new(None) };
    }

    #[cfg(windows)]
    pub(super) fn on_replace_retry() {
        RETRY_HOOK.with(|hook| {
            if let Some(hook) = hook.borrow_mut().as_mut() {
                hook();
            }
        });
    }

    /// A read handle that refuses delete sharing — what a C runtime `open()`
    /// (every CPython `open()`) holds: read and write sharing, never delete.
    #[cfg(windows)]
    fn open_without_delete_sharing(path: &Path) -> std::fs::File {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_SHARE_READ: u32 = 0x1;
        const FILE_SHARE_WRITE: u32 = 0x2;
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(path)
            .unwrap()
    }

    /// The windows write survives a reader that holds the target open without
    /// delete sharing: the first rename is refused (the hook fires — the
    /// condition is real, not assumed), the reader closes, and the retry
    /// lands the new bytes. Red-verify: drop the retry loop and the write
    /// returns the sharing violation.
    #[cfg(windows)]
    #[test]
    fn a_reader_without_delete_sharing_delays_the_replace_but_never_loses_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret");
        write_secret_file_0600(&path, b"old").unwrap();

        let reader = std::rc::Rc::new(std::cell::RefCell::new(Some(open_without_delete_sharing(
            &path,
        ))));
        let held = reader.clone();
        let fired = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let count = fired.clone();
        RETRY_HOOK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                count.set(count.get() + 1);
                held.borrow_mut().take();
            }));
        });

        let written = write_secret_file_0600(&path, b"new");
        RETRY_HOOK.with(|hook| *hook.borrow_mut() = None);

        assert!(
            fired.get() >= 1,
            "the open reader must refuse the first rename — otherwise this test \
             proves nothing about the retry"
        );
        written.expect("the retry must land the write once the reader closes");
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert!(reader.borrow().is_none());
    }

    /// The retry is bounded: a reader that never closes gets the refusal back
    /// once the budget is spent, the old bytes stay whole, and no temp file is
    /// left behind.
    #[cfg(windows)]
    #[test]
    fn a_reader_that_never_closes_fails_the_replace_after_the_budget() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret");
        write_secret_file_0600(&path, b"old").unwrap();
        let _reader = open_without_delete_sharing(&path);

        let started = std::time::Instant::now();
        let err = write_secret_file_0600(&path, b"new").unwrap_err();
        assert!(
            started.elapsed() >= REPLACE_RETRY_BUDGET,
            "a refusal must be retried for the whole budget before it is returned; \
             gave up after {:?} with {err:#}",
            started.elapsed()
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"old");
        let mut tmp_name = path.as_os_str().to_os_string();
        tmp_name.push(".tmp");
        assert!(!std::path::Path::new(&tmp_name).exists());
    }

    /// Structural proof of atomicity: a rewrite replaces the file via a NEW
    /// inode (temp-file-then-rename), never truncates the live path in
    /// place. Truncating in place would keep the same inode; witnessing the
    /// crash WINDOW itself would be a wall-clock-dependent, brittle test, so
    /// this asserts the mechanism headlessly instead.
    #[test]
    fn replaces_via_rename_not_in_place_truncate() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;

            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("secret");

            write_secret_file_0600(&path, b"first").unwrap();
            let ino_before = std::fs::metadata(&path).unwrap().ino();

            write_secret_file_0600(&path, &[9u8; 32]).unwrap();

            let ino_after = std::fs::metadata(&path).unwrap().ino();
            assert_ne!(
                ino_before, ino_after,
                "a rewrite must replace the file via rename(2) onto a fresh inode, never \
                 truncate-and-rewrite the live path in place — otherwise a crash between the \
                 truncate and the write can leave the file short"
            );
            // No stray temp file left behind after a clean rename.
            let mut tmp_name = path.as_os_str().to_os_string();
            tmp_name.push(".tmp");
            assert!(!std::path::Path::new(&tmp_name).exists());
        }
    }

    /// , case 2 of the probe:
    /// a `.tmp` residue from a crash between the open and the rename leaves
    /// a file at whatever mode it was created with — here `0666`, an older
    /// build's `write` or a restored backup. `.mode(0o600)` binds only at
    /// CREATION, so a plain `create(true)` reopen would inherit `0666`
    /// straight through the rename. Red-verify: swap `create_new` back to
    /// `create(true).truncate(true)` and this test reds while
    /// `replaces_via_rename_not_in_place_truncate` above stays green (it
    /// never stages a pre-existing `.tmp`).
    #[test]
    fn narrows_a_pre_existing_wider_mode_tmp_residue() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            use std::os::unix::fs::OpenOptionsExt as _;
            use std::os::unix::fs::PermissionsExt as _;

            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("secret");
            let mut tmp_name = path.as_os_str().to_os_string();
            tmp_name.push(".tmp");
            let tmp_path = PathBuf::from(&tmp_name);

            // Stage the crash residue: a `.tmp` sibling at a wider mode,
            // holding bytes this write must never let survive the rename.
            // `open`'s own `.mode()` argument is masked by the process
            // umask, so an explicit `set_permissions` afterward is what
            // actually guarantees the exact staged mode this test needs.
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o666)
                .open(&tmp_path)
                .unwrap();
            std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o666)).unwrap();
            assert_eq!(
                std::fs::metadata(&tmp_path).unwrap().mode() & 0o777,
                0o666,
                "beside-control: the staged residue must actually be wider than 0600"
            );

            write_secret_file_0600(&path, b"fresh secret").unwrap();

            let mode = std::fs::metadata(&path).unwrap().mode() & 0o777;
            assert_eq!(
                mode, 0o600,
                "a wider-mode .tmp residue must never survive into the live \
                 path — got mode {mode:o}"
            );
            assert_eq!(std::fs::read(&path).unwrap(), b"fresh secret");
        }
    }

    /// , case 3 of the probe: a
    /// `.tmp` residue that is a SYMLINK. A plain `create(true)` open follows
    /// a pre-existing symlink and writes through it to whatever it points
    /// at; `create_new` (`O_CREAT|O_EXCL`) refuses if anything — file or
    /// symlink — already sits at the path, so the unlink-first step must
    /// remove the link before the write, never write through it.
    #[test]
    fn does_not_write_through_a_pre_existing_tmp_symlink() {
        #[cfg(unix)]
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("secret");
            let mut tmp_name = path.as_os_str().to_os_string();
            tmp_name.push(".tmp");
            let tmp_path = PathBuf::from(&tmp_name);

            // A symlink target OUTSIDE this write's own path — if the open
            // ever followed it, the victim file would receive the secret.
            let victim = dir.path().join("victim-file");
            std::fs::write(&victim, b"pre-existing, must stay untouched").unwrap();
            std::os::unix::fs::symlink(&victim, &tmp_path).unwrap();

            write_secret_file_0600(&path, b"fresh secret").unwrap();

            assert_eq!(
                std::fs::read(&victim).unwrap(),
                b"pre-existing, must stay untouched",
                "the symlink target must never be written through"
            );
            assert!(
                !std::path::Path::new(&tmp_path).exists()
                    || std::fs::symlink_metadata(&tmp_path)
                        .map(|m| !m.file_type().is_symlink())
                        .unwrap_or(true),
                "no dangling symlink should survive at the .tmp path either"
            );
            assert_eq!(std::fs::read(&path).unwrap(), b"fresh secret");
        }
    }

    #[cfg(unix)]
    #[test]
    fn mints_owner_only_mode_on_a_fresh_write() {
        use std::os::unix::fs::MetadataExt as _;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret");
        write_secret_file_0600(&path, b"fresh").unwrap();
        let mode = std::fs::metadata(&path).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o600, "a fresh write must be owner-only");
    }

    /// The property `fauna_credential_store::cred_file_write` relies on: two
    /// callers writing the same final `path` through DIFFERENT `tmp_path`
    /// values never collide (each stages its own bytes at its own sibling),
    /// and the final content is whichever call's rename lands last — the
    /// same last-writer-wins contract a single shared `<path>.tmp` would
    /// give a *serialized* pair of writers, without forcing every unlocked
    /// caller onto one shared temp name.
    #[test]
    fn distinct_tmp_paths_never_collide_and_leave_no_residue() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret");
        let tmp_a = dir.path().join("secret.pid1.0.tmp");
        let tmp_b = dir.path().join("secret.pid2.0.tmp");

        write_secret_file_0600_at(&path, &tmp_a, b"from writer a").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"from writer a");
        assert!(
            !tmp_a.exists(),
            "writer a's own tmp must not survive its rename"
        );

        write_secret_file_0600_at(&path, &tmp_b, b"from writer b").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"from writer b");
        assert!(
            !tmp_b.exists(),
            "writer b's own tmp must not survive its rename"
        );
    }
}
