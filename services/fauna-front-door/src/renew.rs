//! Certificate acquisition + renewal over the shared HTTP-01 flow
//! (`fauna-acme-http01`), honoring the persisted failed-validation budget,
//! hot-swapping the resolver on success. Policy: front-door.md § TLS policy
//! (HTTP-01 only, one multi-SAN order, ~30-day renewal lead).

use std::sync::Arc;
use std::time::Duration;

use fauna_acme_http01::{
    ChallengeState, Http01Config, LETS_ENCRYPT_STAGING_URL, ReloadableCertResolver, RetryState,
    cert_covers_sans, cert_seconds_remaining, next_attempt_delay, now_unix, obtain_certificate,
};

use crate::DoorConfig;
use crate::tls::issued_paths;

/// Re-issue when the chain has less than this long to live (the nest's lead).
pub const RENEW_LEAD_SECS: i64 = 30 * 24 * 3600;
/// Steady-state re-check cadence while the certificate is healthy.
pub const HEALTHY_CHECK_INTERVAL: Duration = Duration::from_secs(6 * 3600);
/// Re-check cadence while issuance is due (budget pacing sits on top).
pub const DUE_RECHECK_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// Pure issuance decision: no chain, a chain missing a desired SAN, or a
/// chain inside the renewal lead all demand an order.
pub fn issuance_due(chain_pem: Option<&[u8]>, sans: &[String]) -> bool {
    match chain_pem {
        None => true,
        Some(pem) => {
            !cert_covers_sans(pem, sans)
                || cert_seconds_remaining(pem).is_none_or(|s| s < RENEW_LEAD_SECS)
        }
    }
}

/// Process-lifetime renewal loop.
pub async fn renewal_task(
    cfg: Arc<DoorConfig>,
    challenge_state: Arc<ChallengeState>,
    resolver: Arc<ReloadableCertResolver>,
) {
    let sans = cfg.sans();
    let (cert_path, key_path) = issued_paths(&cfg.state_dir);
    // An explicit directory override wins; else `FAUNA_ACME_STAGING=1` targets Let's
    // Encrypt staging; else the library's Let's Encrypt production.
    let directory_url = cfg.acme_directory_url.clone().or_else(|| {
        cfg.acme_staging
            .then(|| LETS_ENCRYPT_STAGING_URL.to_string())
    });
    let http01 = Http01Config::new(
        cfg.apex.clone(),
        cfg.state_dir.clone(),
        cfg.http_bind.port(),
        directory_url,
    )
    .with_contact_email(cfg.contact_email.clone());
    loop {
        let chain = tokio::fs::read(&cert_path).await.ok();
        if !issuance_due(chain.as_deref(), &sans) {
            tokio::time::sleep(HEALTHY_CHECK_INTERVAL).await;
            continue;
        }

        // Respect the persisted failed-validation budget (resumed across
        // restarts, exactly like the nest's lifecycle task).
        let mut retry = RetryState::load(&cfg.state_dir);
        let wait = next_attempt_delay(&retry, now_unix());
        if !wait.is_zero() {
            tracing::info!(?wait, "ACME retry budget pacing — waiting");
            tokio::time::sleep(wait).await;
        }
        match obtain_certificate(&http01, &sans, &challenge_state, &cfg.state_dir).await {
            Ok(()) => {
                retry.reset();
                retry.save(&cfg.state_dir);
                match resolver.reload(&cert_path, &key_path) {
                    Ok(()) => tracing::info!("certificate issued and hot-swapped"),
                    Err(e) => tracing::error!("issued cert failed to load: {e}"),
                }
            }
            Err(e) => {
                tracing::warn!("ACME issuance failed: {e:#}");
                retry.record_failure(now_unix());
                retry.save(&cfg.state_dir);
                tokio::time::sleep(DUE_RECHECK_INTERVAL).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_acme_http01::test_support::make_multi_san_cert_pem;

    fn sans() -> Vec<String> {
        vec!["door.test".into(), "www.door.test".into()]
    }

    #[test]
    fn missing_chain_is_due() {
        assert!(issuance_due(None, &sans()));
    }

    #[test]
    fn chain_missing_a_san_is_due() {
        let pem = make_multi_san_cert_pem(&["door.test"]);
        assert!(issuance_due(Some(&pem), &sans()));
    }

    #[test]
    fn near_expiry_is_due_and_a_long_lived_covering_chain_is_not() {
        // rcgen's default validity is comfortably beyond the 30-day lead, so
        // a fresh covering chain is NOT due…
        let healthy = make_multi_san_cert_pem(&["door.test", "www.door.test"]);
        assert!(!issuance_due(Some(&healthy), &sans()));

        // …while a chain expiring within the lead is.
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(sans().to_vec()).unwrap();
        params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(1);
        let cert = params.self_signed(&key).unwrap();
        assert!(issuance_due(Some(cert.pem().as_bytes()), &sans()));
    }

    #[test]
    fn unparseable_chain_is_due() {
        assert!(issuance_due(Some(b"not a pem"), &sans()));
    }
}
