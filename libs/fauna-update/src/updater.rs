//! Background update loop: periodic check, download, verify, apply.

use anyhow::Result;
use semver::Version;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::apply::{cleanup_stale, download_to_temp, replace_binary};
use crate::github::check_latest;
use crate::verify::{verify_sha256, verify_signature};
use crate::{RELEASE_PUBLIC_KEY, UpdateConfig, UpdateStatus};

/// Check GitHub Releases for a newer version.
pub async fn check(config: &UpdateConfig) -> Result<UpdateStatus> {
    let client = reqwest::Client::new();
    let current = Version::parse(config.current_version)?;

    match check_latest(
        &client,
        config.github_repo,
        &current,
        config.artifact_prefix,
        env!("TARGET"),
        config.github_token.as_deref(),
    )
    .await?
    {
        Some(info) => Ok(UpdateStatus::Available {
            version: info.version.to_string(),
            release_url: info.release_url,
        }),
        None => Ok(UpdateStatus::UpToDate),
    }
}

/// Download, verify, and replace the binary.
pub async fn apply(config: &UpdateConfig) -> Result<UpdateStatus> {
    let client = reqwest::Client::new();
    let current = Version::parse(config.current_version)?;

    let info = match check_latest(
        &client,
        config.github_repo,
        &current,
        config.artifact_prefix,
        env!("TARGET"),
        config.github_token.as_deref(),
    )
    .await?
    {
        Some(info) => info,
        None => return Ok(UpdateStatus::UpToDate),
    };

    tracing::info!(version = %info.version, "downloading update");

    // Download artifact
    let artifact_path = download_to_temp(&client, &info.artifact_url, &config.install_dir).await?;
    let artifact_bytes = std::fs::read(&artifact_path)?;

    // Download and verify signature
    let sig_resp = client
        .get(&info.signature_url)
        .header("User-Agent", "fauna-update")
        .send()
        .await?;
    let sig_bytes = sig_resp.bytes().await?;

    verify_signature(&artifact_bytes, &sig_bytes, &[RELEASE_PUBLIC_KEY])?;
    tracing::debug!("Ed25519 signature verified");

    // Verify SHA-256 (best-effort)
    if let Some(sums_url) = &info.sums_url {
        match client
            .get(sums_url)
            .header("User-Agent", "fauna-update")
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                let sums = resp.text().await?;
                let artifact_name = info.artifact_url.rsplit('/').next().unwrap_or("unknown");
                if let Err(e) = verify_sha256(&artifact_bytes, artifact_name, &sums) {
                    let _ = std::fs::remove_file(&artifact_path);
                    return Err(e);
                }
                tracing::debug!("SHA-256 checksum verified");
            }
            _ => {
                tracing::warn!("could not download SHA256SUMS — proceeding with Ed25519 only");
            }
        }
    }

    // Replace binary
    let current_exe = std::env::current_exe()?;
    replace_binary(&current_exe, &artifact_path)?;

    let version = info.version.to_string();
    tracing::info!(version = %version, "updated successfully — restarting");

    Ok(UpdateStatus::Applied { version })
}

/// Backoff cap: 168x the base check interval (7 days at a 24h base).
const MAX_BACKOFF_MULTIPLIER: u32 = 168;

/// Double the backoff multiplier, capped at `max`.
fn next_backoff_multiplier(current: u32, max: u32) -> u32 {
    (current * 2).min(max)
}

/// Whether `e` came from `verify_signature`'s failure path — the only error
/// that triggers backoff rather than an immediate retry next interval.
fn is_signature_verification_error(e: &anyhow::Error) -> bool {
    e.to_string().contains("signature verification failed")
}

/// Spawn a background update loop.
///
/// Returns a `watch::Receiver` that callers can subscribe to for status
/// updates. The receiver holds `None` until the first check completes.
///
/// If `config.auto_apply` is `true`, new versions are downloaded and applied
/// automatically, then `std::process::exit(0)` is called so the process
/// manager can restart with the new binary. If `auto_apply` is `false`, the
/// status is published on the watch channel and the caller decides when to act.
///
/// Signature verification failures trigger exponential backoff (doubling the
/// check interval, capped at 168× the base interval — 7 days at a 24 h base).
/// Other errors are logged and the loop continues without resetting backoff.
/// A successful `UpToDate` result resets backoff to 1×.
pub fn spawn_update_loop(
    config: UpdateConfig,
    shutdown: CancellationToken,
) -> watch::Receiver<Option<UpdateStatus>> {
    let (tx, rx) = watch::channel(None);

    tokio::spawn(async move {
        let _ = cleanup_stale(&config.install_dir);

        let mut interval = tokio::time::interval(config.check_interval);
        let mut backoff_multiplier: u32 = 1;

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    tracing::debug!("update loop shutting down");
                    break;
                }
                _ = interval.tick() => {}
            }

            match check(&config).await {
                Ok(UpdateStatus::Available {
                    ref version,
                    ref release_url,
                }) => {
                    tracing::warn!(
                        version = %version,
                        url = %release_url,
                        "new version available"
                    );

                    if config.auto_apply {
                        match apply(&config).await {
                            Ok(UpdateStatus::Applied { version }) => {
                                tracing::info!(version = %version, "update applied, exiting for restart");
                                std::process::exit(0);
                            }
                            Ok(_) => {}
                            Err(e) => {
                                if is_signature_verification_error(&e) {
                                    backoff_multiplier = next_backoff_multiplier(
                                        backoff_multiplier,
                                        MAX_BACKOFF_MULTIPLIER,
                                    );
                                    let next = config.check_interval * backoff_multiplier;
                                    interval = tokio::time::interval(next);
                                    tracing::error!(
                                        error = %e,
                                        next_check_secs = next.as_secs(),
                                        "signature verification failed — backing off"
                                    );
                                } else {
                                    tracing::error!(error = %e, "update apply failed");
                                }
                                // Do NOT reset backoff on error
                                continue;
                            }
                        }
                    } else {
                        let _ = tx.send(Some(UpdateStatus::Available {
                            version: version.clone(),
                            release_url: release_url.clone(),
                        }));
                    }
                }
                Ok(UpdateStatus::UpToDate) => {
                    tracing::debug!("up to date");
                    let _ = tx.send(Some(UpdateStatus::UpToDate));
                    // Reset backoff on successful up-to-date check
                    backoff_multiplier = 1;
                    interval = tokio::time::interval(config.check_interval);
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "update check failed");
                }
            }
        }
    });

    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_backoff_multiplier_doubles() {
        assert_eq!(next_backoff_multiplier(1, 168), 2);
        assert_eq!(next_backoff_multiplier(2, 168), 4);
        assert_eq!(next_backoff_multiplier(42, 168), 84);
    }

    #[test]
    fn next_backoff_multiplier_clamps_exactly_at_the_cap() {
        assert_eq!(next_backoff_multiplier(84, 168), 168);
    }

    #[test]
    fn next_backoff_multiplier_clamps_when_doubling_would_overshoot() {
        assert_eq!(next_backoff_multiplier(100, 168), 168);
    }

    #[test]
    fn next_backoff_multiplier_stays_capped_once_already_at_the_cap() {
        assert_eq!(next_backoff_multiplier(168, 168), 168);
        assert_eq!(next_backoff_multiplier(200, 168), 168);
    }

    #[test]
    fn is_signature_verification_error_matches_the_verify_module_message() {
        // Exact message verify_signature::bail!s on (libs/fauna-update/src/verify.rs).
        let e = anyhow::anyhow!("signature verification failed: no matching key");
        assert!(is_signature_verification_error(&e));
    }

    #[test]
    fn is_signature_verification_error_is_false_for_unrelated_errors() {
        let e = anyhow::anyhow!("network error: connection reset");
        assert!(!is_signature_verification_error(&e));
    }

    #[test]
    fn is_signature_verification_error_does_not_match_a_partial_substring() {
        // Guards against a future message rewording silently losing the
        // backoff trigger (e.g. dropping "verification" or "failed").
        let e = anyhow::anyhow!("signature failed");
        assert!(!is_signature_verification_error(&e));
        let e = anyhow::anyhow!("verification failed");
        assert!(!is_signature_verification_error(&e));
    }
}
