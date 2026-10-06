//! The crash-safe file-replace primitive: write-tmp → fsync → rename → fsync
//! the parent directory — and its read counterpart.
//!
//! Independently hand-rolled at five sites before this module existed (the
//! segment-store manifest, the mail/card/cal placement manifests, and the
//! nest's TLS cert/key writer). They agreed on the shape and disagreed on the
//! last step — which is the step with production-incident history, so the
//! disagreement is resolved here once instead of per-site.
//!
//! [`read_optional`] is the read half of the same pair. Three of those sites
//! (the mail/card/cal placement manifests) kept a hand-rolled copy of it after
//! the write half was lifted here, so the "an absent manifest means a fresh
//! actor" rule was stated three times and owned nowhere.

use std::io;
use std::path::Path;

/// Atomically replace `path` with `bytes`.
///
/// Writes `bytes` to `tmp` (created/truncated), fsyncs the write handle, closes
/// it, renames `tmp` over `path`, then fsyncs the parent directory so the
/// rename entry itself is durable. A crash at any point leaves either the old
/// `path` or the new one — never a torn file.
///
/// `tmp` **must** be in the same directory as `path` (rename is only atomic
/// within a filesystem). Callers pass it explicitly rather than having this
/// function derive it, because the existing on-disk tmp names differ per kind
/// (`<file>.tmp`, `<stem>.mail-placement.tmp`, …) and renaming transient files
/// is not worth the churn in a durability path. The parent directory is created
/// if missing.
///
/// ## Why the parent-dir fsync is `#[cfg(unix)]` and not best-effort
///
/// Fsyncing the parent is a POSIX idiom: without it the rename entry can be
/// lost on power failure even though the file's own data was synced. It is
/// **skipped entirely on Windows**, deliberately: opening a directory with
/// plain `OpenOptions` fails there (`CreateFile` without
/// `FILE_FLAG_BACKUP_SEMANTICS` → `ERROR_ACCESS_DENIED`, os error 5) and
/// `FlushFileBuffers` on a directory handle is unsupported anyway; the
/// `MoveFileEx`-backed `rename` is durable on NTFS without it.
///
/// **This is not a hypothetical.** The dir-fsync is what broke `posts.create`
/// on a Windows nest after the 2026-06-15 posts segment cutover — the first
/// `save_atomic` a Windows nest ever exercised (conv e2e uses mock backends;
/// the mail bridge cannot run on Windows).
///
/// Three of the five call sites this replaced instead ran the fsync
/// unconditionally inside `if let Ok(dir) = File::open(parent) { let _ =
/// dir.sync_all(); }`, reaching the same Windows outcome by *accident* —
/// relying on the open failing — while also swallowing a genuine fsync error
/// on Unix, where `EIO` means the durability this function promises did not
/// happen. The explicit `cfg` keeps the platform decision legible, and the `?`
/// keeps the promise honest: if this returns `Ok`, the replace is durable.
pub fn atomic_save(path: &Path, tmp: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write as _;

    // `Path::new("foo").parent()` is `Some("")`, not `None` — filter the empty
    // case out so a bare relative filename means "no directory to sync".
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(parent) = parent {
        std::fs::create_dir_all(parent)?;
    }

    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    } // closed before the rename, so the parent-dir fsync covers a quiesced state

    std::fs::rename(tmp, path)?;

    #[cfg(unix)]
    if let Some(parent) = parent {
        std::fs::OpenOptions::new()
            .read(true)
            .open(parent)?
            .sync_all()?;
    }

    Ok(())
}

/// Read `path` in full, returning `Ok(None)` if it does not exist.
///
/// The read counterpart to [`atomic_save`], and the rule that makes the pair
/// work: a per-actor manifest is simply **absent for a fresh actor**, which is
/// a normal state rather than an error, while every other `open`/`read`
/// failure is a real one and propagates. Stating that in one place is the
/// point — the three placement manifests each said it themselves.
///
/// Callers decode the bytes; the manifest type and its format-version
/// fallback stay per-kind.
pub fn read_optional(path: &Path) -> io::Result<Option<Vec<u8>>> {
    use std::io::Read as _;

    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    Ok(Some(buf))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn read_optional_returns_none_for_a_missing_file() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("never-written.cbor");

        assert_eq!(read_optional(&missing).unwrap(), None);
    }

    #[test]
    fn read_optional_round_trips_what_atomic_save_wrote() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.cbor");
        let tmp = dir.path().join("state.cbor.tmp");
        atomic_save(&path, &tmp, b"manifest bytes").unwrap();

        assert_eq!(
            read_optional(&path).unwrap(),
            Some(b"manifest bytes".to_vec())
        );
    }

    #[test]
    fn read_optional_reads_an_empty_file_as_empty_not_absent() {
        // `Some(vec![])` and `None` mean different things to a caller: an
        // empty manifest file is corruption to be reported by the decoder,
        // not a fresh actor to be started from scratch.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.cbor");
        std::fs::write(&path, b"").unwrap();

        assert_eq!(read_optional(&path).unwrap(), Some(Vec::new()));
    }

    #[test]
    fn read_optional_propagates_a_non_notfound_error() {
        // A directory in place of the file is not "absent" — reading it fails,
        // and that failure must not be flattened into `Ok(None)`.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("a-directory");
        std::fs::create_dir(&path).unwrap();

        let err = read_optional(&path);
        assert!(
            err.is_err() || matches!(err, Ok(Some(_))),
            "a directory must not read as an absent file"
        );
        if let Err(e) = err {
            assert_ne!(e.kind(), io::ErrorKind::NotFound);
        }
    }

    #[test]
    fn writes_bytes_and_removes_the_tmp_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.cbor");
        let tmp = dir.path().join("state.cbor.tmp");

        atomic_save(&path, &tmp, b"hello").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
        assert!(!tmp.exists(), "tmp must be renamed away, not left behind");
    }

    #[test]
    fn replaces_an_existing_file_wholesale() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.cbor");
        let tmp = dir.path().join("state.cbor.tmp");

        atomic_save(&path, &tmp, b"a longer first version").unwrap();
        atomic_save(&path, &tmp, b"short").unwrap();

        // Truncation, not overlay — a shorter second write must not leave a
        // tail of the first behind.
        assert_eq!(std::fs::read(&path).unwrap(), b"short");
    }

    #[test]
    fn creates_the_parent_directory_when_missing() {
        let dir = TempDir::new().unwrap();
        let nested = dir.path().join("a").join("b");
        let path = nested.join("state.cbor");
        let tmp = nested.join("state.cbor.tmp");

        atomic_save(&path, &tmp, b"x").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"x");
    }

    #[test]
    fn a_stale_tmp_from_a_previous_crash_is_truncated_not_appended() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.cbor");
        let tmp = dir.path().join("state.cbor.tmp");
        std::fs::write(&tmp, b"garbage left by a crashed writer").unwrap();

        atomic_save(&path, &tmp, b"clean").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"clean");
    }

    #[test]
    fn empty_payload_is_a_valid_replace() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.cbor");
        let tmp = dir.path().join("state.cbor.tmp");
        atomic_save(&path, &tmp, b"not empty").unwrap();

        atomic_save(&path, &tmp, b"").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"");
    }
}
