//! Platform-specific binary replacement for self-updates.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Clean up stale files from previous updates.
pub fn cleanup_stale(install_dir: &Path) -> Result<()> {
    if let Ok(entries) = std::fs::read_dir(install_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.ends_with(".old") || name_str.ends_with(".fauna-update-tmp") {
                if let Err(e) = std::fs::remove_file(entry.path()) {
                    tracing::debug!(path = %entry.path().display(), error = %e, "failed to clean up stale file");
                } else {
                    tracing::debug!(path = %entry.path().display(), "cleaned up stale update file");
                }
            }
        }
    }
    Ok(())
}

/// Download an artifact from a URL to a temp file in the install directory.
pub async fn download_to_temp(
    client: &reqwest::Client,
    url: &str,
    install_dir: &Path,
) -> Result<PathBuf> {
    let resp = client
        .get(url)
        .header("User-Agent", "fauna-update")
        .send()
        .await
        .context("download failed")?;

    if !resp.status().is_success() {
        anyhow::bail!("download returned {}: {url}", resp.status());
    }

    let bytes = resp.bytes().await.context("reading response body")?;

    std::fs::create_dir_all(install_dir)
        .with_context(|| format!("creating install dir {}", install_dir.display()))?;

    let tmp_path = install_dir.join(format!(
        "fauna-update-{}.fauna-update-tmp",
        std::process::id()
    ));
    std::fs::write(&tmp_path, &bytes)
        .with_context(|| format!("writing temp file {}", tmp_path.display()))?;

    Ok(tmp_path)
}

/// Replace the current binary with the new one.
pub fn replace_binary(current_binary: &Path, new_binary: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(new_binary, perms).context("setting executable permission")?;

        std::fs::rename(new_binary, current_binary).with_context(|| {
            format!(
                "renaming {} -> {}",
                new_binary.display(),
                current_binary.display()
            )
        })?;
    }

    #[cfg(windows)]
    {
        let old_path = current_binary.with_extension("exe.old");
        let _ = std::fs::remove_file(&old_path);

        std::fs::rename(current_binary, &old_path)
            .with_context(|| format!("renaming current binary to {}", old_path.display()))?;

        std::fs::rename(new_binary, current_binary)
            .with_context(|| format!("renaming new binary to {}", current_binary.display()))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn cleanup_removes_old_files() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("fauna-sync.exe.old"), "old").unwrap();
        std::fs::write(dir.path().join("fauna-update-123.fauna-update-tmp"), "tmp").unwrap();
        std::fs::write(dir.path().join("fauna-sync"), "keep").unwrap();

        cleanup_stale(dir.path()).unwrap();

        assert!(!dir.path().join("fauna-sync.exe.old").exists());
        assert!(
            !dir.path()
                .join("fauna-update-123.fauna-update-tmp")
                .exists()
        );
        assert!(dir.path().join("fauna-sync").exists());
    }

    #[test]
    #[cfg(unix)]
    fn replace_binary_atomic_rename() {
        let dir = TempDir::new().unwrap();
        let current = dir.path().join("fauna-sync");
        let new = dir.path().join("fauna-sync.new");

        std::fs::write(&current, "v1").unwrap();
        std::fs::write(&new, "v2").unwrap();

        replace_binary(&current, &new).unwrap();

        assert_eq!(std::fs::read_to_string(&current).unwrap(), "v2");
        assert!(!new.exists());
    }

    #[tokio::test]
    async fn download_to_temp_writes_the_response_body() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/artifact.bin"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"release bytes".to_vec()))
            .mount(&server)
            .await;

        let dir = TempDir::new().unwrap();
        let client = reqwest::Client::new();
        let url = format!("{}/artifact.bin", server.uri());

        let tmp_path = download_to_temp(&client, &url, dir.path()).await.unwrap();

        assert!(tmp_path.starts_with(dir.path()));
        assert_eq!(std::fs::read(&tmp_path).unwrap(), b"release bytes");
    }

    #[tokio::test]
    async fn download_to_temp_rejects_a_non_success_status_and_writes_nothing() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/artifact.bin"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let dir = TempDir::new().unwrap();
        let client = reqwest::Client::new();
        let url = format!("{}/artifact.bin", server.uri());

        let err = download_to_temp(&client, &url, dir.path())
            .await
            .expect_err("a 404 must not be treated as a successful download");
        assert!(err.to_string().contains("404"));

        // The guard fires before any write, so a failed download must never
        // leave a stray temp file behind for a later step to mistake as real.
        assert!(
            std::fs::read_dir(dir.path()).unwrap().next().is_none(),
            "a rejected download must not write a temp file"
        );
    }
}
