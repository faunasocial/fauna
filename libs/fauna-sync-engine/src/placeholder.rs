//! Are a file's bytes actually **on this disk**?
//!
//! On an on-demand root (Windows cfapi today; a macOS File Provider or a Linux FUSE host
//! tomorrow — `docs/goal/behavior/file-sync.md` § On-Demand Files) a *tracked* file can exist
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

/// `FILE_ATTRIBUTE_OFFLINE` — the file's data is not immediately available.
#[cfg(windows)]
const FILE_ATTRIBUTE_OFFLINE: u32 = 0x0000_1000;

/// `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS` — a cloud-provider placeholder whose content is
/// fetched from the provider when read.
#[cfg(windows)]
const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;

/// True when `meta` describes a file whose **bytes are not on this disk** — see the module doc.
///
/// Always `false` off Windows: no other platform fauna ships on has a placeholder surface yet,
/// so every file there is genuinely local. When one lands (a macOS File Provider domain, a Linux
/// FUSE host), this is the single function that learns to recognize it, and every caller —
/// scan, reconcile, upload — inherits the guard for free.
#[cfg(windows)]
pub fn is_cloud_placeholder(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes() & (FILE_ATTRIBUTE_OFFLINE | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS) != 0
}

/// True when `meta` describes a file whose **bytes are not on this disk** — see the module doc.
#[cfg(not(windows))]
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
    ///
    /// Declared here rather than pulling the `windows` crate into a cross-platform shared crate
    /// for a single test call.
    #[cfg(windows)]
    #[test]
    fn a_file_the_os_marks_offline_is_a_placeholder() {
        use std::os::windows::ffi::OsStrExt;

        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn SetFileAttributesW(path: *const u16, attrs: u32) -> i32;
        }

        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("offline.txt");
        std::fs::write(&f, b"content the OS will claim is elsewhere").unwrap();
        assert!(!path_is_cloud_placeholder(&f), "precondition: starts local");

        let wide: Vec<u16> = f.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: `wide` is a NUL-terminated UTF-16 path living until after the call.
        let rc = unsafe { SetFileAttributesW(wide.as_ptr(), FILE_ATTRIBUTE_OFFLINE) };
        assert_ne!(rc, 0, "SetFileAttributesW(OFFLINE) failed");

        assert!(
            path_is_cloud_placeholder(&f),
            "a file whose bytes the OS says are not here must never be hashed or uploaded"
        );
    }

    /// A path that does not exist is not a placeholder — the caller's missing-file handling
    /// stays in charge.
    #[test]
    fn a_missing_path_is_not_a_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!path_is_cloud_placeholder(&dir.path().join("nope.txt")));
    }
}
