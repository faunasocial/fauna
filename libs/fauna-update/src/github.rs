//! GitHub Releases API client for version discovery.

use anyhow::{Context, Result, bail};
use semver::Version;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    published_at: Option<String>,
    assets: Vec<Asset>,
}

#[derive(Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// Information about an available release.
#[derive(Debug, Clone)]
pub struct ReleaseInfo {
    pub version: Version,
    pub release_url: String,
    pub published_at: Option<String>,
    pub artifact_url: String,
    pub signature_url: String,
    pub sums_url: Option<String>,
}

/// Check GitHub Releases for a version newer than `current`.
///
/// Returns `None` if the current version is up to date or newer.
pub async fn check_latest(
    client: &reqwest::Client,
    repo: &str,
    current: &Version,
    artifact_prefix: &str,
    target: &str,
    token: Option<&str>,
) -> Result<Option<ReleaseInfo>> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");

    let mut req = client
        .get(&url)
        .header("User-Agent", "fauna-update")
        .header("Accept", "application/vnd.github+json");

    if let Some(t) = token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }

    let resp = req.send().await.context("GitHub API request failed")?;

    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        tracing::debug!("no releases found for {repo}");
        return Ok(None);
    }

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("GitHub API returned {status}: {body}");
    }

    let release: Release = resp.json().await.context("parsing release JSON")?;

    resolve_release_info(release, current, artifact_prefix, target)
}

/// Decide whether `release` is an upgrade over `current`, and if so resolve
/// which of its assets are this platform's artifact/signature/sums. Pulled
/// out of [`check_latest`] as the pure (network-free) half of that function.
fn resolve_release_info(
    release: Release,
    current: &Version,
    artifact_prefix: &str,
    target: &str,
) -> Result<Option<ReleaseInfo>> {
    let tag = release
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&release.tag_name);
    let remote_version = Version::parse(tag)
        .with_context(|| format!("invalid semver in tag: {}", release.tag_name))?;

    // Downgrade protection: only update if strictly newer
    if remote_version <= *current {
        return Ok(None);
    }

    // Find the matching artifact for this platform
    let artifact_name = format!("{artifact_prefix}-v{remote_version}-{target}");
    let artifact = release
        .assets
        .iter()
        .find(|a| a.name == artifact_name || a.name == format!("{artifact_name}.exe"))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no artifact matching '{artifact_name}' in release {}",
                release.tag_name
            )
        })?;

    let sig_name = format!("{}.sig", artifact.name);
    let signature = release
        .assets
        .iter()
        .find(|a| a.name == sig_name)
        .ok_or_else(|| anyhow::anyhow!("no signature file '{sig_name}' in release"))?;

    let sums = release
        .assets
        .iter()
        .find(|a| a.name == "SHA256SUMS")
        .map(|a| a.browser_download_url.clone());

    Ok(Some(ReleaseInfo {
        version: remote_version,
        release_url: release.html_url,
        published_at: release.published_at,
        artifact_url: artifact.browser_download_url.clone(),
        signature_url: signature.browser_download_url.clone(),
        sums_url: sums,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_comparison_blocks_downgrade() {
        let current = Version::parse("1.0.0").unwrap();
        let older = Version::parse("0.9.0").unwrap();
        assert!(older <= current);
    }

    #[test]
    fn version_comparison_allows_upgrade() {
        let current = Version::parse("1.0.0").unwrap();
        let newer = Version::parse("1.1.0").unwrap();
        assert!(newer > current);
    }

    fn asset(name: &str) -> Asset {
        Asset {
            name: name.to_string(),
            browser_download_url: format!("https://example.com/{name}"),
        }
    }

    fn release(tag_name: &str, assets: Vec<Asset>) -> Release {
        Release {
            tag_name: tag_name.to_string(),
            html_url: "https://example.com/releases/latest".to_string(),
            published_at: Some("2026-07-18T00:00:00Z".to_string()),
            assets,
        }
    }

    const PREFIX: &str = "fauna-desktop";
    const TARGET: &str = "x86_64-unknown-linux-gnu";

    fn full_asset_set(version: &str) -> Vec<Asset> {
        let artifact_name = format!("{PREFIX}-v{version}-{TARGET}");
        vec![
            asset(&artifact_name),
            asset(&format!("{artifact_name}.sig")),
            asset("SHA256SUMS"),
        ]
    }

    #[test]
    fn resolve_release_info_blocks_a_downgrade() {
        let current = Version::parse("2.0.0").unwrap();
        let r = release("v1.9.0", full_asset_set("1.9.0"));
        let result = resolve_release_info(r, &current, PREFIX, TARGET).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn resolve_release_info_blocks_the_same_version() {
        let current = Version::parse("1.0.0").unwrap();
        let r = release("v1.0.0", full_asset_set("1.0.0"));
        let result = resolve_release_info(r, &current, PREFIX, TARGET).unwrap();
        assert!(result.is_none(), "equal version must not be an upgrade");
    }

    #[test]
    fn resolve_release_info_strips_the_v_prefix() {
        let current = Version::parse("1.0.0").unwrap();
        let r = release("v1.2.3", full_asset_set("1.2.3"));
        let info = resolve_release_info(r, &current, PREFIX, TARGET)
            .unwrap()
            .unwrap();
        assert_eq!(info.version, Version::parse("1.2.3").unwrap());
    }

    #[test]
    fn resolve_release_info_accepts_a_tag_without_v_prefix() {
        let current = Version::parse("1.0.0").unwrap();
        let r = release("1.2.3", full_asset_set("1.2.3"));
        let info = resolve_release_info(r, &current, PREFIX, TARGET)
            .unwrap()
            .unwrap();
        assert_eq!(info.version, Version::parse("1.2.3").unwrap());
    }

    #[test]
    fn resolve_release_info_errors_on_invalid_semver_tag() {
        let current = Version::parse("1.0.0").unwrap();
        let r = release("not-a-version", vec![]);
        let err = resolve_release_info(r, &current, PREFIX, TARGET).unwrap_err();
        assert!(err.to_string().contains("invalid semver"));
    }

    #[test]
    fn resolve_release_info_finds_the_matching_artifact() {
        let current = Version::parse("1.0.0").unwrap();
        let r = release("v1.2.3", full_asset_set("1.2.3"));
        let info = resolve_release_info(r, &current, PREFIX, TARGET)
            .unwrap()
            .unwrap();
        assert_eq!(
            info.artifact_url,
            format!("https://example.com/{PREFIX}-v1.2.3-{TARGET}")
        );
        assert_eq!(
            info.signature_url,
            format!("https://example.com/{PREFIX}-v1.2.3-{TARGET}.sig")
        );
        assert_eq!(
            info.sums_url.as_deref(),
            Some("https://example.com/SHA256SUMS")
        );
        assert_eq!(info.release_url, "https://example.com/releases/latest");
        assert_eq!(info.published_at.as_deref(), Some("2026-07-18T00:00:00Z"));
    }

    #[test]
    fn resolve_release_info_falls_back_to_the_exe_suffixed_artifact_name() {
        let current = Version::parse("1.0.0").unwrap();
        let artifact_name = format!("{PREFIX}-v1.2.3-{TARGET}");
        let r = release(
            "v1.2.3",
            vec![
                asset(&format!("{artifact_name}.exe")),
                asset(&format!("{artifact_name}.exe.sig")),
            ],
        );
        let info = resolve_release_info(r, &current, PREFIX, TARGET)
            .unwrap()
            .unwrap();
        assert_eq!(
            info.artifact_url,
            format!("https://example.com/{artifact_name}.exe")
        );
        assert_eq!(info.sums_url, None, "no SHA256SUMS asset was provided");
    }

    #[test]
    fn resolve_release_info_errors_when_no_matching_artifact() {
        let current = Version::parse("1.0.0").unwrap();
        let r = release("v1.2.3", vec![asset("some-other-binary")]);
        let err = resolve_release_info(r, &current, PREFIX, TARGET).unwrap_err();
        assert!(err.to_string().contains("no artifact matching"));
    }

    #[test]
    fn resolve_release_info_errors_when_signature_asset_is_missing() {
        let current = Version::parse("1.0.0").unwrap();
        let artifact_name = format!("{PREFIX}-v1.2.3-{TARGET}");
        let r = release("v1.2.3", vec![asset(&artifact_name)]);
        let err = resolve_release_info(r, &current, PREFIX, TARGET).unwrap_err();
        assert!(err.to_string().contains("no signature file"));
    }
}
