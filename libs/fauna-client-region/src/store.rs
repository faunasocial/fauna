//! Where a native app keeps the device record ([`crate::RegionPlane::to_bytes`])
//! — one file under the app's install-scoped config dir, the same relative path
//! on every native shell (`region-blocking.md` § How an app obtains its region's
//! policy: the at-rest *format* is shared Rust's, the bytes live in each shell's
//! own install-scoped state). Install-scoped because a region is a fact about
//! the device, not the account: the record survives sign-out and identity
//! switch. Its bytes are public signed documents plus their replay floors,
//! never a secret.
//!
//! Web has no filesystem; its shell keeps the same bytes in browser storage.

use std::path::{Path, PathBuf};

/// The record's path under an app's config dir.
pub fn record_path(config_dir: &Path) -> PathBuf {
    config_dir.join("region").join("content-policy.cbor")
}

/// The stored record, or `None` when there is none, or the file itself cannot
/// be read. Bytes that do not decode are handed over as they are:
/// [`crate::RegionPlane::load`] holds nothing from them and never writes over
/// them.
pub fn read_record(path: &Path) -> Option<Vec<u8>> {
    std::fs::read(path).ok()
}

/// Replace the record — write-then-rename, so a crash mid-write leaves the
/// previous record, never a torn one. An empty `bytes` (the plane could not
/// encode) writes nothing: persisting nothing is the `no information`
/// direction.
pub fn write_record(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("cbor.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}
