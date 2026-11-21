//! Are a file's bytes actually **on this disk**?
//!
//! On an on-demand root (Windows cfapi; on apple, a bound folder inside another provider's
//! File Provider tree — iCloud Drive, a `~/Library/CloudStorage` domain — whose evicted files are
//! *dataless*; a Linux FUSE host tomorrow — `docs/goal/behavior/on-demand-files.md` § On-Demand
//! Files, `docs/goal/behavior/on-demand-files-apple.md` § Apple File Provider binding) a *tracked*
//! file can exist
//! with the right name, size and mtime while its content lives only on the nest. That
//! **cloud-only placeholder** is `present-but-unreadable`, and getting the distinction wrong
//! breaks the engine in two opposite directions — both of which have bitten (2026-07-14):
//!
//! 1. **Never read one.** cfapi fires no callback for I/O originating in the *provider's own
//!    process*, and the sync service **is** the provider. So its own read of its own
//!    placeholder is answered by nobody: it blocks for cfapi's full **60-second** timeout and
//!    then fails with `ERROR_CLOUD_FILE_REQUEST_TIMEOUT` (os error 426) — measured, not
//!    inferred. Hashing or uploading one burns those 60 s on the single driving thread that
//!    also *serves* hydration, so an N-placeholder set spends N × 60 s unable to hydrate
//!    anything, while every log line still reads "reconciliation complete".
//!
//! 2. **Never mistake one for absent.** It is *present*. A scan that simply omits placeholders
//!    makes `reconcile`'s delete-detection see a `Synced` row with no file behind it, and
//!    record a **delete on the nest** — propagating the deletion to every other device. A user
//!    reaches exactly that state with Explorer's ordinary *"Free up space"*. Freeing disk space
//!    must never mean erasing the file.
//!
//! So a placeholder is reported by the scan (satisfying 2) and carries a flag (satisfying 1).
//!
//! **This needs no cfapi dependency.** The OS already says it in the ordinary file attributes,
//! which `std` exposes — so the shared engine can ask the question on every platform without
//! taking a Windows-only crate.
//!
//! Measured on a live sync root, and the reason the predicate below is exactly these two bits:
//!
//! | state | attributes | OFFLINE | RECALL_ON_DATA_ACCESS |
//! |---|---|---|---|
//! | cloud-only placeholder | `0x00401620` | **yes** | **yes** |
//! | hydrated | `0x00000420` | no | no |
//! | overwritten by another process | `0x00000020` | no | no |
//!
//! The hydrated and edited rows are why the guard cannot suppress the uploads two-way sync
//! exists to deliver: once the bytes are local, neither bit is set and the file is ordinary.
//!
//! **On apple the same question is one `st_flags` bit.** A File Provider evicts a file by
//! making it *dataless* — `SF_DATALESS` (`0x40000000`, `<sys/stat.h>`) set in the `stat` flags,
//! the bytes on the provider's side. `stat` reports the flag without materializing the file;
//! an `open` + `read` is what makes the OS download it — for a whole bound folder at the first
//! scan, or on a peer's behalf at a serve ask. The rule is windows' unchanged: reported, never
//! read, never absent. Userland cannot set the bit (`chflags` refuses it), so the predicate is
//! pinned over a synthetic flags value ([`flags_are_dataless`]).

/// `FILE_ATTRIBUTE_OFFLINE` — the file's data is not immediately available.
#[cfg(windows)]
const FILE_ATTRIBUTE_OFFLINE: u32 = 0x0000_1000;

/// `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS` — a cloud-provider placeholder whose content is
/// fetched from the provider when read.
#[cfg(windows)]
const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;

/// `SF_DATALESS` — the file's data is held by its File Provider, not on this disk
/// (`<sys/stat.h>`, apple only).
#[cfg(target_vendor = "apple")]
const SF_DATALESS: u32 = 0x4000_0000;

/// True when an apple `st_flags` value marks a **dataless** file — see the module doc.
#[cfg(target_vendor = "apple")]
fn flags_are_dataless(flags: u32) -> bool {
    flags & SF_DATALESS != 0
}

/// True when `meta` describes a file whose **bytes are not on this disk** — see the module doc.
///
/// This is the single function a platform's placeholder is taught to, and every caller —
/// scan, reconcile, upload, the watcher, the serve's tree source — inherits the guard for free:
/// windows reads the cfapi attribute bits, macOS and iOS the `SF_DATALESS` flag. Everywhere
/// else (Linux, Android, the other apple OSes) it is `false`. On Linux that is the ruled
/// answer, not a gap: fauna's own FUSE root keeps only local bytes in its underlying directory,
/// and a bound folder inside another service's mount carries no per-file attribute to read
/// (`docs/goal/behavior/on-demand-files-linux.md` § Linux FUSE binding).
#[cfg(windows)]
pub fn is_cloud_placeholder(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes() & (FILE_ATTRIBUTE_OFFLINE | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS) != 0
}

/// True when `meta` describes a file whose **bytes are not on this disk** — see the module doc.
#[cfg(target_os = "macos")]
pub fn is_cloud_placeholder(meta: &std::fs::Metadata) -> bool {
    use std::os::macos::fs::MetadataExt;
    flags_are_dataless(meta.st_flags())
}

/// True when `meta` describes a file whose **bytes are not on this disk** — see the module doc.
#[cfg(target_os = "ios")]
pub fn is_cloud_placeholder(meta: &std::fs::Metadata) -> bool {
    use std::os::ios::fs::MetadataExt;
    flags_are_dataless(meta.st_flags())
}

/// True when `meta` describes a file whose **bytes are not on this disk** — see the module doc.
#[cfg(not(any(windows, target_os = "macos", target_os = "ios")))]
pub fn is_cloud_placeholder(_meta: &std::fs::Metadata) -> bool {
    false
}

/// [`is_cloud_placeholder`] for a path that has not been `stat`ed yet.
///
/// A path that cannot be `stat`ed at all is reported `false` — "not a placeholder" — so the
/// caller's own missing-file handling stays in charge of it rather than this predicate quietly
/// reinterpreting a vanished file as a cloud-only one.
pub fn path_is_cloud_placeholder(path: &std::path::Path) -> bool {
    std::fs::metadata(path)
        .map(|m| is_cloud_placeholder(&m))
        .unwrap_or(false)
}

/// Set `FILE_ATTRIBUTE_OFFLINE` on `path` (`offline`), or put it back to an ordinary file's
/// attributes (`!offline`, the bytes "back on this disk"). `OFFLINE` is the half of a cfapi
/// cloud-only placeholder's attributes an ordinary process may set, so a test's file reads as
/// cloud-only to [`path_is_cloud_placeholder`] with no sync root and no cfapi — the engine's
/// tests' one way to make a placeholder. The live end-to-end proof against a *real* cfapi
/// placeholder lives in `fauna-sync-agent`'s `cfapi_live_integration`.
///
/// Declared here rather than pulling the `windows` crate into a cross-platform shared crate
/// for a test call.
#[cfg(all(test, windows))]
pub(crate) fn set_offline_for_test(path: &std::path::Path, offline: bool) {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetFileAttributesW(path: *const u16, attrs: u32) -> i32;
    }
    const FILE_ATTRIBUTE_NORMAL: u32 = 0x0000_0080;

    let attrs = if offline {
        FILE_ATTRIBUTE_OFFLINE
    } else {
        FILE_ATTRIBUTE_NORMAL
    };
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` is a NUL-terminated UTF-16 path living until after the call.
    let rc = unsafe { SetFileAttributesW(wide.as_ptr(), attrs) };
    assert_ne!(rc, 0, "SetFileAttributesW({attrs:#x}) failed");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An ordinary local file is never a placeholder — the case that must keep working, or
    /// two-way sync would silently stop uploading everything.
    #[test]
    fn an_ordinary_file_is_not_a_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("real.txt");
        std::fs::write(&f, b"bytes that are genuinely here").unwrap();

        assert!(!path_is_cloud_placeholder(&f));
        assert!(!is_cloud_placeholder(&std::fs::metadata(&f).unwrap()));
    }

    /// A file the OS marks `OFFLINE` is a placeholder. cfapi sets that bit together with
    /// `RECALL_ON_DATA_ACCESS` (measured `0x00401620`), and `OFFLINE` is the half an ordinary
    /// process is allowed to set — so this pins the predicate cheaply, with no sync root and no
    /// cfapi. The live end-to-end proof against a *real* cfapi placeholder lives in
    /// `fauna-sync-agent`'s `cfapi_live_integration`.
    #[cfg(windows)]
    #[test]
    fn a_file_the_os_marks_offline_is_a_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("offline.txt");
        std::fs::write(&f, b"content the OS will claim is elsewhere").unwrap();
        assert!(!path_is_cloud_placeholder(&f), "precondition: starts local");

        set_offline_for_test(&f, true);

        assert!(
            path_is_cloud_placeholder(&f),
            "a file whose bytes the OS says are not here must never be hashed or uploaded"
        );
    }

    /// The `SF_DATALESS` bit marks a placeholder, alone or among the other flags a provider's
    /// file carries — the value a File Provider leaves on an evicted file.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_dataless_flag_marks_a_placeholder() {
        const UF_HIDDEN: u32 = 0x0000_8000;
        const UF_TRACKED: u32 = 0x0000_0040;
        assert!(flags_are_dataless(SF_DATALESS));
        assert!(flags_are_dataless(SF_DATALESS | UF_TRACKED | UF_HIDDEN));
    }

    /// An ordinary flags value is no placeholder — the other user and system flags never are,
    /// or two-way sync would silently stop uploading a hidden or tracked file.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn an_ordinary_flags_value_is_not() {
        const UF_NODUMP: u32 = 0x0000_0001;
        const UF_HIDDEN: u32 = 0x0000_8000;
        const UF_TRACKED: u32 = 0x0000_0040;
        const SF_ARCHIVED: u32 = 0x0001_0000;
        for flags in [
            0,
            UF_NODUMP,
            UF_HIDDEN,
            UF_TRACKED,
            SF_ARCHIVED,
            !SF_DATALESS,
        ] {
            assert!(!flags_are_dataless(flags), "{flags:#x}");
        }
    }

    /// A path that does not exist is not a placeholder — the caller's missing-file handling
    /// stays in charge.
    #[test]
    fn a_missing_path_is_not_a_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!path_is_cloud_placeholder(&dir.path().join("nope.txt")));
    }
}
