//! Crash-safe file writing via write-to-temp + fsync + rename.

use std::path::Path;

use anyhow::{Context, Result};

/// Write `data` to `target` atomically.
///
/// Writes to a temporary file in the same directory, fsyncs, then renames.
/// If the process crashes mid-write, the original file is untouched.
pub async fn atomic_write_file(target: &Path, data: &[u8]) -> Result<()> {
    // Redacted once and reused by every error string below (`path-sealing.md`
    // § Sealed names & paths, S7): `target` is call-site-agnostic — some
    // callers pass a user-chosen folder's own file path — so this shared
    // primitive redacts unconditionally rather than trusting every caller to
    // wrap its own context.
    let target_r = fauna_core::log_redact::log_path(&target.to_string_lossy());
    let parent = target
        .parent()
        .context("target path has no parent directory")?;

    // Ensure parent exists
    tokio::fs::create_dir_all(parent)
        .await
        .with_context(|| format!("creating parent dirs for {target_r}"))?;

    // Write to a temp file in the same directory (same filesystem for rename)
    let temp = tempfile::NamedTempFile::new_in(parent).with_context(|| {
        format!(
            "creating temp file in {}",
            fauna_core::log_redact::log_path(&parent.to_string_lossy())
        )
    })?;
    let temp_path = temp.path().to_path_buf();

    // Write data
    tokio::fs::write(&temp_path, data).await.with_context(|| {
        format!(
            "writing temp file {}",
            fauna_core::log_redact::log_path(&temp_path.to_string_lossy())
        )
    })?;

    // Fsync the file (open with write access — required on Windows)
    {
        let file = tokio::fs::OpenOptions::new()
            .write(true)
            .open(&temp_path)
            .await
            .context("opening temp file for fsync")?;
        file.sync_all().await.context("fsync temp file")?;
    } // file handle dropped before rename

    // Atomic rename
    temp.into_temp_path()
        .persist(target)
        .with_context(|| format!("renaming temp file to {target_r}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn atomic_write_creates_file_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("output.txt");
        let data = b"hello world";

        atomic_write_file(&target, data).await.unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), data);
    }

    #[tokio::test]
    async fn atomic_write_replaces_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("output.txt");
        std::fs::write(&target, b"old content").unwrap();

        atomic_write_file(&target, b"new content").await.unwrap();

        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new content");
    }
}
