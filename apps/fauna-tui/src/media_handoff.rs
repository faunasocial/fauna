//! The external-media handoff's plaintext materialization — the hardened temp
//! file of `apps/tui.md` § External media handoff (ratified 2026-07-16;
//! loopback streaming rejected there, with the threat model).
//!
//! The decrypted clip must reach a program that cannot decrypt (the OS default
//! media handler), so it rests briefly as a file. This module owns everything
//! about that file so the boundary has one auditable home:
//!
//! - **Location** — the most private, most ephemeral per-user dir the platform
//!   offers ([`handoff_dir`]): on Linux `$XDG_RUNTIME_DIR/fauna-tui/media/`
//!   (tmpfs — the plaintext never touches persistent disk), falling back to a
//!   per-uid dir under the system temp dir; on macOS/Windows the per-user temp.
//! - **Permissions** — the dir is created `0700`; each file `0600` with
//!   create-new semantics (never reused, never truncating a pre-placed path).
//! - **Naming** — a unique stem plus the item's **real extension** (the OS
//!   resolves the handler by extension, so it is load-bearing).
//! - **Lifetime** — event-driven, not player-exit-driven (the opener detaches,
//!   so the player's lifetime is unknowable): [`sweep`] runs at app start, app
//!   exit, sign-out and factory reset, and each new [`materialize`] deletes
//!   prior handoff files first — at most one clip rests at a time. POSIX
//!   unlink-while-open is safe (the player keeps its fd); a Windows
//!   delete-while-open fails harmlessly and the next sweep collects it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Monotonic disambiguator for same-nanosecond materializations (one process
/// writes at most one clip at a time, but cheap insurance is cheap).
static SEQ: AtomicU64 = AtomicU64::new(0);

/// The handoff directory — created on demand by [`materialize`]; `None` only
/// when the platform offers no per-user base at all (then the handoff is
/// unavailable and the caller surfaces that on `error-message`).
///
/// Linux/BSD prefer `$XDG_RUNTIME_DIR` (tmpfs, per-user `0700`, cleared at
/// logout); a headless box without one falls back to `{temp}/fauna-tui-media-{uid}`
/// (uid-suffixed so two users on one box never contest the same path). macOS's
/// `std::env::temp_dir()` is already the per-user Darwin temp, Windows's the
/// per-user `%TEMP%` — both take the plain `fauna-tui/media` subdir.
pub fn handoff_dir() -> Option<PathBuf> {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
            return Some(PathBuf::from(runtime).join("fauna-tui").join("media"));
        }
        // SAFETY: getuid is always safe to call.
        let uid = unsafe { libc::getuid() };
        Some(std::env::temp_dir().join(format!("fauna-tui-media-{uid}")))
    }
    #[cfg(any(not(unix), target_os = "macos"))]
    {
        Some(std::env::temp_dir().join("fauna-tui").join("media"))
    }
}

/// Delete every file in the handoff dir, best-effort — the app-start / app-exit
/// / sign-out / factory-reset sweep. Missing dir, undeletable entries (a
/// Windows player still holding one open) and IO errors are all fine: the next
/// sweep collects what this one could not.
pub fn sweep() {
    let Some(dir) = handoff_dir() else { return };
    sweep_dir(&dir);
}

fn sweep_dir(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let _ = std::fs::remove_file(entry.path());
    }
}

/// Write `bytes` as a fresh handoff file named after `item_name`'s extension,
/// deleting prior handoff files first; returns the path to hand the OS opener.
pub fn materialize(item_name: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    let dir = handoff_dir().ok_or_else(|| {
        std::io::Error::other("no per-user temp location available for the media handoff")
    })?;
    materialize_in(&dir, item_name, bytes)
}

/// [`materialize`]'s core with the dir injected (the unit-testable seam — the
/// public fn resolves the real environment's dir, which a test must not touch).
///
/// The dir is created `0700` (owner-only before any plaintext exists in it);
/// the file is `0600` + create-new. Any failure returns the error for the
/// page's `error-message` — never a partial file handed to the player (the
/// write completes and syncs before the path is returned).
fn materialize_in(dir: &Path, item_name: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    create_private_dir(dir)?;
    // Prior-handoff-first: at most one decrypted clip rests at a time.
    sweep_dir(dir);

    // The extension is what the OS handler resolution keys on; keep the real
    // one. The stem is ours (unique per call) — never the item name, which
    // could collide or carry path-hostile characters.
    let extension = Path::new(item_name)
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let mut file_name = format!("clip-{nanos:x}-{seq:x}");
    if !extension.is_empty() {
        file_name.push('.');
        file_name.push_str(&extension);
    }
    let path = dir.join(file_name);

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    use std::io::Write;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(path)
}

/// Create `dir` (and parents) with owner-only permissions on the leaf.
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole contract in one pass: exact bytes, owner-only file + dir, the
    /// item's real extension on a non-item-derived stem.
    #[test]
    fn materialize_writes_a_private_file_with_the_real_extension() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("handoff");

        let path = materialize_in(&dir, "Holiday video.mp4", b"plaintext clip bytes").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"plaintext clip bytes");
        assert_eq!(path.extension().unwrap(), "mp4");
        let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
        assert!(
            !stem.contains("Holiday"),
            "the stem is ours, never the item name: {stem}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let file_mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(file_mode & 0o777, 0o600, "owner-only file");
            let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode();
            assert_eq!(dir_mode & 0o777, 0o700, "owner-only dir");
        }
    }

    /// Each new handoff deletes the prior one first — at most one decrypted
    /// clip rests at a time (the ratified lifetime rule).
    #[test]
    fn a_new_handoff_sweeps_the_prior_one() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("handoff");

        let first = materialize_in(&dir, "a.mp4", b"one").unwrap();
        let second = materialize_in(&dir, "b.mp3", b"two").unwrap();
        assert!(!first.exists(), "the prior clip is deleted");
        assert!(second.exists());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    }

    /// An extension-less item name still materializes (no trailing dot).
    #[test]
    fn an_extension_less_name_gets_a_bare_stem() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("handoff");
        let path = materialize_in(&dir, "Makefile", b"x").unwrap();
        assert!(path.extension().is_none());
        assert!(!path.to_string_lossy().ends_with('.'));
    }

    /// The sweep is best-effort: a missing dir is a silent no-op, never a panic.
    #[test]
    fn sweeping_a_missing_dir_is_a_no_op() {
        sweep_dir(Path::new("/definitely/not/a/real/dir"));
    }
}
