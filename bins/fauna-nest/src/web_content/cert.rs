//! Per-custom-domain TLS certificate lifecycle for web-content hosting.
//!
//! Mirrors [`acme_http01::cert_lifecycle_loop`](crate::acme_http01) but
//! per-active-web-domain. Each pass it reconciles the set of `active` rows in
//! `web_domains` against the live [`MultiDomainCertResolver`]:
//!
//! - **Issue / renew** — for each `active` domain whose per-domain cert is
//!   missing or within 30 days of expiry, run an HTTP-01 order (the shared
//!   [`obtain_certificate`] + [`ChallengeState`] / `start_http01_listener` —
//!   challenges keyed by token, so concurrent multi-domain orders are safe),
//!   write the cert to `acme_dir/<domain>/`, then install it into the resolver
//!   via [`add_domain`](MultiDomainCertResolver::add_domain). No filesystem
//!   watcher: the apex [`cert_watcher_task`](crate::acme::cert_watcher_task)
//!   deliberately watches only the default-cert dir.
//! - **Re-install** — a fresh on-disk cert that the resolver doesn't yet hold
//!   (e.g. after a restart, when the resolver starts empty) is loaded from disk
//!   and installed without re-issuing.
//! - **Remove** — a domain that has a cert in the resolver but is no longer
//!   `active` (deregistered via `fauna.web.domain.delete` → `delete_web_domain`,
//!   so the row is gone) is dropped from the resolver and its `acme_dir/<domain>/`
//!   deleted. This single reconciliation loop is the sole authority over the
//!   resolver's per-domain map.
//!
//! Custom web-domain certs are a **distinct cert population** from the nest's
//! own apex (+ `mail.<domain>`) cert: they SHARE the apex Let's Encrypt account
//! (`acme_dir/account-key.json`, passed to [`obtain_certificate`] as
//! `account_dir`) — one account, per-domain cert files — but stay OUT of
//! `desired_san_domains` and out of `store_acme_material` (the bridge
//! seal-and-fan-out). See `docs/goal/behavior/web-content-hosting.md`
//! § Separation from the nest's own / mail-bridge certs.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::acme::{CERT_FILENAME, KEY_FILENAME, MultiDomainCertResolver, load_certified_key};
use crate::acme_http01::{
    ChallengeState, Http01Config, RetryState, cert_seconds_remaining, next_attempt_delay, now_unix,
    obtain_certificate,
};
use crate::db::CacheDb;
use crate::web_content::serve::HostResolver;

/// Renew when fewer than 30 days remain — same threshold as the apex cert
/// (`acme_http01::cert_lifecycle_loop`).
const RENEWAL_THRESHOLD_SECS: i64 = 30 * 24 * 60 * 60;

/// Steady poll cadence: re-reconcile every 5 minutes so a freshly-`active`
/// domain gets a cert promptly and a deregistered one is dropped promptly.
const STEADY_POLL: Duration = Duration::from_secs(5 * 60);

/// The per-domain subdir under the apex acme dir where this domain's cert files
/// (`fullchain.pem` / `privkey.pem`) and its failed-validation retry state
/// (`acme-retry-state.json`) live. The shared LE account lives one level up, in
/// the apex `acme_dir` itself.
fn domain_dir(acme_dir: &Path, domain: &str) -> PathBuf {
    acme_dir.join(domain)
}

/// Config for the per-domain web-content cert lifecycle.
#[derive(Debug, Clone)]
pub struct WebCertConfig {
    /// Apex ACME dir. Per-domain certs go in `<acme_dir>/<domain>/`; the LE
    /// account in `<acme_dir>/account-key.json` is SHARED across all custom
    /// domains (one account, not one per domain — shared rate budget).
    pub acme_dir: PathBuf,
    /// Explicit ACME directory URL override (`[acme].directory_url`, shared with
    /// the apex cert). `None` ⇒ Let's Encrypt production.
    pub directory_url: Option<String>,
}

/// Production entry point: issue via the real ACME HTTP-01 flow and run forever.
/// The loop body lives in [`web_cert_lifecycle_loop`] so tests drive it with a
/// fake issuer (real ACME can't be faked).
///
/// `challenge_state` MUST be the same [`ChallengeState`] the HTTP-01 listener
/// serves (the apex `acme_http01_runtime` one), so per-domain challenges are
/// reachable at `http://<domain>/.well-known/acme-challenge/<token>` on the
/// shared port-80 listener. Challenges are keyed by token, so the apex cert
/// loop and this loop can run orders concurrently without collision.
///
/// `host_resolver` is the router's live [`HostResolver`] — the loop reads the
/// apex from it each tick (`nest_domain()`, empty ⇒ no node domain ⇒ subdomain
/// hosting off, only custom domains reconcile — `web-content-hosting.md`
/// § Architectural rule 8), so per-subdomain issuance follows a **post-boot**
/// domain claim exactly when routing does (`apply_primary_identity` swaps the
/// one value both read; `domains-and-tls-bootstrap.md` § Implementation status).
/// A separate param, not a `WebCertConfig` field: the resolver holds `RwLock`s,
/// which would break the config's `Debug`/`Clone` derives.
pub async fn web_cert_lifecycle_task(
    db: Arc<CacheDb>,
    resolver: Arc<MultiDomainCertResolver>,
    challenge_state: Arc<ChallengeState>,
    config: WebCertConfig,
    host_resolver: Arc<HostResolver>,
) {
    let acme_dir = config.acme_dir.clone();
    let issue = move |domain: String| {
        let config = config.clone();
        let challenge_state = challenge_state.clone();
        async move {
            // Per-domain cert files go in the per-domain subdir; the LE account
            // is shared from the apex acme_dir (`account_dir`), so a nest with
            // N custom domains has ONE Let's Encrypt account.
            let http01 = Http01Config::new(
                domain.clone(),
                config.acme_dir.join(&domain),
                8080,
                config.directory_url.clone(),
            );
            obtain_certificate(
                &http01,
                std::slice::from_ref(&domain),
                &challenge_state,
                &config.acme_dir,
            )
            .await
        }
    };
    web_cert_lifecycle_loop(db, resolver, acme_dir, host_resolver, issue, None).await;
}

/// The per-domain cert-lifecycle loop body, parameterised over the issuance
/// operation and an optional iteration cap. [`web_cert_lifecycle_task`] is the
/// production caller (real `obtain_certificate`, `max_iterations = None` → runs
/// forever); tests pass a fake issuer (which writes a cert into the per-domain
/// dir, exactly as `obtain_certificate` does) and a small cap. `issue` receives
/// the domain name and is responsible for writing `acme_dir/<domain>/{fullchain,
/// privkey}.pem`; the loop then loads that cert and installs it.
async fn web_cert_lifecycle_loop<I, Fut>(
    db: Arc<CacheDb>,
    resolver: Arc<MultiDomainCertResolver>,
    acme_dir: PathBuf,
    host_resolver: Arc<HostResolver>,
    issue: I,
    max_iterations: Option<usize>,
) where
    I: Fn(String) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>>,
{
    let mut iterations: usize = 0;
    loop {
        // Re-read the apex each tick: a post-boot claim swaps the router's
        // `HostResolver` domain (`apply_primary_identity`), and the next pass
        // here picks it up — per-subdomain issuance follows the claim with no
        // restart, like routing.
        let nest_domain = host_resolver.nest_domain();
        reconcile_once(&db, &resolver, &acme_dir, &nest_domain, &issue).await;

        iterations += 1;
        if let Some(max) = max_iterations
            && iterations >= max
        {
            break;
        }
        tokio::time::sleep(STEADY_POLL).await;
    }
}

/// The set of `<handle>.<nest_domain>` FQDNs that should currently have a
/// per-subdomain cert: every actor opted into subdomain hosting
/// ([`list_subdomain_enabled`](CacheDb::list_subdomain_enabled)) that has a
/// non-empty, non-reserved handle. An opted-in actor with no handle (or a
/// reserved-label handle like `mail`/`app`) is skipped — there is nothing to
/// serve, so nothing to issue. A propagated DB error makes the caller skip the
/// whole pass (never partially drop live certs).
async fn opted_in_subdomain_fqdns(db: &CacheDb, nest_domain: &str) -> anyhow::Result<Vec<String>> {
    let actors = db.list_subdomain_enabled().await?;
    let mut out = Vec::new();
    for actor in actors {
        if let Some(handle) = db.get_handle(&actor).await?
            && !handle.is_empty()
            && !crate::web_content::serve::is_reserved_subdomain_label(&handle)
        {
            out.push(format!("{handle}.{nest_domain}"));
        }
    }
    Ok(out)
}

/// One reconciliation pass (factored out of the loop so it's directly testable
/// and so a DB-read failure short-circuits the pass without wiping resolver
/// state).
async fn reconcile_once<I, Fut>(
    db: &CacheDb,
    resolver: &MultiDomainCertResolver,
    acme_dir: &Path,
    nest_domain: &str,
    issue: &I,
) where
    I: Fn(String) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>>,
{
    // The full domain list. On a DB error we MUST NOT proceed — an empty
    // `active` set would make the removal step drop every per-domain cert.
    let rows = match db.list_all_web_domains().await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!("web_cert: list_all_web_domains failed, skipping pass: {e:#}");
            return;
        }
    };
    let mut active: Vec<String> = rows
        .into_iter()
        .filter(|r| r.status == "active")
        .map(|r| r.domain)
        .collect();

    // Augment with opted-in subdomain certs (`<handle>.<nest_domain>`) — the same
    // reconcile owns their add/renew/remove, so they share the removal-drop +
    // backoff machinery (web-content-hosting.md § Architectural rule 8). They
    // stay OUT of `desired_san_domains` / `store_acme_material` (rule 5) by
    // construction: this loop never touches those. Skipped entirely with no node
    // domain (localhost/no-domain deploy → no subdomain hosting). On a DB error
    // we skip the WHOLE pass for the same reason as above (an incomplete `active`
    // set would wrongly drop live subdomain certs).
    if !nest_domain.is_empty() {
        match opted_in_subdomain_fqdns(db, nest_domain).await {
            Ok(mut subs) => active.append(&mut subs),
            Err(e) => {
                tracing::warn!("web_cert: list opted-in subdomains failed, skipping pass: {e:#}");
                return;
            }
        }
    }

    // Removal: any domain the resolver holds that is no longer active (the row
    // was deleted, or moved out of `active`) is dropped and its dir removed.
    for domain in resolver.domain_names() {
        if !active.iter().any(|d| d == &domain) {
            resolver.remove_domain(&domain);
            let dir = domain_dir(acme_dir, &domain);
            if dir.exists()
                && let Err(e) = tokio::fs::remove_dir_all(&dir).await
            {
                tracing::warn!("web_cert: remove dir {} failed: {e}", dir.display());
            }
            tracing::info!("web_cert: dropped deregistered domain {domain}");
        }
    }

    // Issuance / renewal / re-install for each active domain.
    for domain in &active {
        let dir = domain_dir(acme_dir, domain);
        let cert_path = dir.join(CERT_FILENAME);
        let key_path = dir.join(KEY_FILENAME);

        let need_issue = match tokio::fs::read(&cert_path).await {
            Ok(pem) => cert_seconds_remaining(&pem)
                .map(|r| r < RENEWAL_THRESHOLD_SECS)
                .unwrap_or(true),
            Err(_) => true,
        };

        if !need_issue {
            // Fresh cert on disk. Make sure the resolver actually holds it —
            // after a restart the resolver starts empty and must be repopulated
            // from disk without burning an issuance.
            if !resolver.has_domain(domain) {
                match load_certified_key(&cert_path, &key_path) {
                    Ok(cert) => {
                        resolver.add_domain(domain, cert);
                        tracing::info!("web_cert: installed existing on-disk cert for {domain}");
                    }
                    Err(e) => {
                        tracing::error!("web_cert: load existing cert for {domain} failed: {e:#}")
                    }
                }
            }
            continue;
        }

        // Ensure the per-domain dir exists so retry-state + cert files can be
        // written even if the first issuance fails before `obtain_certificate`
        // creates it.
        if let Err(e) = tokio::fs::create_dir_all(&dir).await {
            tracing::error!("web_cert: create dir {} failed: {e}", dir.display());
            continue;
        }

        // Per-domain failed-validation budget — the same rolling-window
        // machinery the apex cert uses (LE's limit is per account, per hostname,
        // per hour). State file lives in the per-domain subdir.
        let mut retry = RetryState::load(&dir);
        let wait = next_attempt_delay(&retry, now_unix());
        if !wait.is_zero() {
            tracing::debug!(
                "web_cert: {domain} within failed-validation backoff ({}s), skipping this pass",
                wait.as_secs()
            );
            continue;
        }

        tracing::info!(domain, "web_cert: attempting issuance");
        match issue(domain.clone()).await {
            Ok(()) => {
                retry.reset();
                retry.save(&dir);
                match load_certified_key(&cert_path, &key_path) {
                    Ok(cert) => {
                        resolver.add_domain(domain, cert);
                        tracing::info!("web_cert: issued + installed cert for {domain}");
                    }
                    Err(e) => tracing::error!(
                        "web_cert: issued cert for {domain} but loading it failed: {e:#}"
                    ),
                }
            }
            Err(e) => {
                retry.record_failure(now_unix());
                retry.save(&dir);
                tracing::error!(
                    domain,
                    failures_this_hour = retry.failures_in_window(now_unix()),
                    "web_cert: issuance failed: {e:#}"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acme::spki_sha256_of_cert_der;
    use rustls::sign::CertifiedKey;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn actor() -> [u8; 32] {
        [0x7Au8; 32]
    }

    fn db() -> Arc<CacheDb> {
        Arc::new(CacheDb::open_in_memory().unwrap())
    }

    /// Register `domain` for the test actor and drive it straight to `active`
    /// (the DNS-verification step is exercised separately in `domain.rs`).
    async fn seed_active_domain(db: &CacheDb, domain: &str) {
        db.insert_web_domain(&actor(), domain, "tok").await.unwrap();
        db.update_web_domain_status(domain, "active").await.unwrap();
    }

    /// Create a handled user and opt them into subdomain hosting — the
    /// `<handle>.<nest_domain>` cert source for Slice 3.
    async fn seed_subdomain_actor(db: &CacheDb, actor: &[u8; 32], handle: &str) {
        db.create_user_with_handle(actor, "free", handle, None)
            .await
            .unwrap();
        db.set_subdomain_enabled(actor).await.unwrap();
    }

    /// Generate a self-signed cert valid for `days` and return its leaf DER plus
    /// the PEM cert + key. `days` controls `not_after` so the renewal threshold
    /// can be exercised.
    fn gen_cert(domain: &str, days: i64) -> (Vec<u8>, String, String) {
        let key = rcgen::KeyPair::generate().expect("keygen");
        let mut params = rcgen::CertificateParams::new(vec![domain.to_string()]).expect("params");
        params.not_before = time::OffsetDateTime::now_utc() - time::Duration::days(1);
        params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(days);
        let cert = params.self_signed(&key).expect("self-sign");
        let der = cert.der().as_ref().to_vec();
        (der, cert.pem(), key.serialize_pem())
    }

    /// Write a cert into the per-domain dir, exactly as `obtain_certificate`
    /// would. Returns the leaf SPKI fingerprint for resolver assertions.
    fn write_cert(acme_dir: &Path, domain: &str, days: i64) -> [u8; 32] {
        let dir = domain_dir(acme_dir, domain);
        std::fs::create_dir_all(&dir).unwrap();
        let (der, cert_pem, key_pem) = gen_cert(domain, days);
        std::fs::write(dir.join(CERT_FILENAME), cert_pem).unwrap();
        std::fs::write(dir.join(KEY_FILENAME), key_pem).unwrap();
        spki_sha256_of_cert_der(&der).unwrap()
    }

    fn spki_of(key: &CertifiedKey) -> [u8; 32] {
        spki_sha256_of_cert_der(key.end_entity_cert().unwrap().as_ref()).unwrap()
    }

    /// A real default (apex) cert so `cert_for_sni` fallbacks are checkable.
    fn default_resolver() -> (Arc<MultiDomainCertResolver>, [u8; 32]) {
        let key = rcgen::KeyPair::generate().unwrap();
        let params = rcgen::CertificateParams::new(vec!["nest.example.com".to_string()]).unwrap();
        let cert = params.self_signed(&key).unwrap();
        let der = cert.der().as_ref().to_vec();
        let cert_obj = load_certified_key_from_pem(&cert.pem(), &key.serialize_pem());
        (
            Arc::new(MultiDomainCertResolver::new(cert_obj)),
            spki_sha256_of_cert_der(&der).unwrap(),
        )
    }

    fn load_certified_key_from_pem(cert_pem: &str, key_pem: &str) -> CertifiedKey {
        use rustls::pki_types::pem::PemObject;
        use rustls::pki_types::{CertificateDer, PrivateKeyDer};

        let certs: Vec<_> = CertificateDer::pem_slice_iter(cert_pem.as_bytes())
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        let key_der = PrivateKeyDer::from_pem_slice(key_pem.as_bytes()).unwrap();
        let signing_key = rustls::crypto::ring::sign::any_supported_type(&key_der).unwrap();
        CertifiedKey::new(certs, signing_key)
    }

    // ---- The success criterion: register → active → issue → resolver serves
    //      the per-domain cert for that SNI and the default for an unknown SNI.

    #[tokio::test]
    async fn issues_and_installs_then_resolver_serves_per_domain_and_default() {
        let dir = tempfile::tempdir().unwrap();
        let acme_dir = dir.path().to_path_buf();
        let db = db();
        seed_active_domain(&db, "alice.com").await;

        let (resolver, default_fp) = default_resolver();

        // Fake issuer: write a real self-signed cert into the per-domain dir,
        // exactly as `obtain_certificate` would, and remember its fingerprint.
        let alice_fp = Arc::new(std::sync::Mutex::new([0u8; 32]));
        let issue = {
            let acme_dir = acme_dir.clone();
            let alice_fp = alice_fp.clone();
            move |domain: String| {
                let acme_dir = acme_dir.clone();
                let alice_fp = alice_fp.clone();
                async move {
                    *alice_fp.lock().unwrap() = write_cert(&acme_dir, &domain, 90);
                    Ok(())
                }
            }
        };

        web_cert_lifecycle_loop(
            db.clone(),
            resolver.clone(),
            acme_dir,
            Arc::new(HostResolver::new(String::new())),
            issue,
            Some(1),
        )
        .await;

        let alice_fp = *alice_fp.lock().unwrap();
        assert_ne!(alice_fp, default_fp);

        // Matching SNI → the per-domain cert.
        assert_eq!(
            spki_of(
                &resolver
                    .cert_for_sni(Some("alice.com"))
                    .expect("alice cert")
            ),
            alice_fp,
            "resolver must serve alice.com's own cert for its SNI"
        );
        // Unknown SNI → the default (apex) cert.
        assert_eq!(
            spki_of(&resolver.cert_for_sni(Some("unknown.com")).expect("default")),
            default_fp,
            "an unknown SNI must fall back to the nest's default cert"
        );
    }

    #[tokio::test]
    async fn skips_issuance_for_a_fresh_already_installed_cert() {
        let dir = tempfile::tempdir().unwrap();
        let acme_dir = dir.path().to_path_buf();
        let db = db();
        seed_active_domain(&db, "alice.com").await;

        // Pre-write a fresh (90-day) cert on disk and pre-install it.
        let fp = write_cert(&acme_dir, "alice.com", 90);
        let (resolver, _default_fp) = default_resolver();
        let cert = load_certified_key(
            &domain_dir(&acme_dir, "alice.com").join(CERT_FILENAME),
            &domain_dir(&acme_dir, "alice.com").join(KEY_FILENAME),
        )
        .unwrap();
        resolver.add_domain("alice.com", cert);

        let calls = Arc::new(AtomicUsize::new(0));
        let issue = {
            let calls = calls.clone();
            move |_domain: String| {
                let calls = calls.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            }
        };

        web_cert_lifecycle_loop(
            db,
            resolver.clone(),
            acme_dir,
            Arc::new(HostResolver::new(String::new())),
            issue,
            Some(1),
        )
        .await;

        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "a fresh cert must not re-issue"
        );
        assert_eq!(
            spki_of(&resolver.cert_for_sni(Some("alice.com")).unwrap()),
            fp
        );
    }

    #[tokio::test]
    async fn reinstalls_fresh_on_disk_cert_after_restart_without_issuing() {
        let dir = tempfile::tempdir().unwrap();
        let acme_dir = dir.path().to_path_buf();
        let db = db();
        seed_active_domain(&db, "alice.com").await;

        // Cert exists on disk but the resolver is empty (cold restart).
        let fp = write_cert(&acme_dir, "alice.com", 90);
        let (resolver, _default_fp) = default_resolver();
        assert!(!resolver.has_domain("alice.com"));

        let calls = Arc::new(AtomicUsize::new(0));
        let issue = {
            let calls = calls.clone();
            move |_domain: String| {
                let calls = calls.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            }
        };

        web_cert_lifecycle_loop(
            db,
            resolver.clone(),
            acme_dir,
            Arc::new(HostResolver::new(String::new())),
            issue,
            Some(1),
        )
        .await;

        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "a fresh cert must be re-installed, not re-issued"
        );
        assert_eq!(
            spki_of(&resolver.cert_for_sni(Some("alice.com")).unwrap()),
            fp,
            "the on-disk cert must be installed into the resolver after a restart"
        );
    }

    #[tokio::test]
    async fn renews_a_near_expiry_cert() {
        let dir = tempfile::tempdir().unwrap();
        let acme_dir = dir.path().to_path_buf();
        let db = db();
        seed_active_domain(&db, "alice.com").await;

        // A cert with only 10 days left — under the 30-day renewal threshold.
        let stale_fp = write_cert(&acme_dir, "alice.com", 10);
        let (resolver, _default_fp) = default_resolver();

        // Issuer writes a fresh 90-day cert (a new key → new fingerprint).
        let fresh_fp = Arc::new(std::sync::Mutex::new([0u8; 32]));
        let calls = Arc::new(AtomicUsize::new(0));
        let issue = {
            let acme_dir = acme_dir.clone();
            let fresh_fp = fresh_fp.clone();
            let calls = calls.clone();
            move |domain: String| {
                let acme_dir = acme_dir.clone();
                let fresh_fp = fresh_fp.clone();
                let calls = calls.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    *fresh_fp.lock().unwrap() = write_cert(&acme_dir, &domain, 90);
                    Ok(())
                }
            }
        };

        web_cert_lifecycle_loop(
            db,
            resolver.clone(),
            acme_dir,
            Arc::new(HostResolver::new(String::new())),
            issue,
            Some(1),
        )
        .await;

        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "near-expiry cert must be renewed"
        );
        let fresh_fp = *fresh_fp.lock().unwrap();
        assert_ne!(fresh_fp, stale_fp);
        assert_eq!(
            spki_of(&resolver.cert_for_sni(Some("alice.com")).unwrap()),
            fresh_fp,
            "the resolver must serve the renewed cert"
        );
    }

    #[tokio::test]
    async fn removes_deregistered_domain_from_resolver_and_disk() {
        let dir = tempfile::tempdir().unwrap();
        let acme_dir = dir.path().to_path_buf();
        let db = db();
        seed_active_domain(&db, "alice.com").await;

        let (resolver, _default_fp) = default_resolver();
        let issue = {
            let acme_dir = acme_dir.clone();
            move |domain: String| {
                let acme_dir = acme_dir.clone();
                async move {
                    write_cert(&acme_dir, &domain, 90);
                    Ok(())
                }
            }
        };

        // Pass 1: issue + install.
        web_cert_lifecycle_loop(
            db.clone(),
            resolver.clone(),
            acme_dir.clone(),
            Arc::new(HostResolver::new(String::new())),
            &issue,
            Some(1),
        )
        .await;
        assert!(resolver.has_domain("alice.com"));
        assert!(domain_dir(&acme_dir, "alice.com").exists());

        // Deregister (client-driven delete removes the row), then reconcile.
        assert!(db.delete_web_domain("alice.com").await.unwrap());
        web_cert_lifecycle_loop(
            db,
            resolver.clone(),
            acme_dir.clone(),
            Arc::new(HostResolver::new(String::new())),
            &issue,
            Some(1),
        )
        .await;

        assert!(
            !resolver.has_domain("alice.com"),
            "a deregistered domain must be dropped from the resolver"
        );
        assert!(
            resolver.cert_for_sni(Some("alice.com")).is_some(),
            "an unknown SNI still falls back to the default cert"
        );
        assert!(
            !domain_dir(&acme_dir, "alice.com").exists(),
            "the per-domain acme dir must be deleted on removal"
        );
    }

    // ---- Slice 3: opted-in subdomains issue + renew + drop just like custom
    //      domains, keyed on `<handle>.<nest_domain>`.

    #[tokio::test]
    async fn issues_per_subdomain_cert_for_opted_in_actor() {
        let dir = tempfile::tempdir().unwrap();
        let acme_dir = dir.path().to_path_buf();
        let db = db();
        // alice opts into subdomain hosting → cert for alice.example.com.
        seed_subdomain_actor(&db, &[0x01u8; 32], "alice").await;
        // bob has a handle but did NOT opt in → no cert.
        db.create_user_with_handle(&[0x02u8; 32], "free", "bob", None)
            .await
            .unwrap();

        let (resolver, default_fp) = default_resolver();
        let issue = {
            let acme_dir = acme_dir.clone();
            move |domain: String| {
                let acme_dir = acme_dir.clone();
                async move {
                    write_cert(&acme_dir, &domain, 90);
                    Ok(())
                }
            }
        };

        web_cert_lifecycle_loop(
            db.clone(),
            resolver.clone(),
            acme_dir,
            Arc::new(HostResolver::new("example.com".to_string())),
            issue,
            Some(1),
        )
        .await;

        // alice.example.com got its own cert; bob.example.com did not (not opted in)
        // → unknown SNI falls back to the default cert.
        assert!(resolver.has_domain("alice.example.com"));
        assert_eq!(
            spki_of(&resolver.cert_for_sni(Some("bob.example.com")).unwrap()),
            default_fp,
            "an actor who did not opt in must not get a subdomain cert"
        );
    }

    #[tokio::test]
    async fn drops_subdomain_cert_when_actor_opts_out() {
        let dir = tempfile::tempdir().unwrap();
        let acme_dir = dir.path().to_path_buf();
        let db = db();
        let alice = [0x01u8; 32];
        seed_subdomain_actor(&db, &alice, "alice").await;

        let (resolver, _default_fp) = default_resolver();
        let issue = {
            let acme_dir = acme_dir.clone();
            move |domain: String| {
                let acme_dir = acme_dir.clone();
                async move {
                    write_cert(&acme_dir, &domain, 90);
                    Ok(())
                }
            }
        };

        // Pass 1: issue + install.
        web_cert_lifecycle_loop(
            db.clone(),
            resolver.clone(),
            acme_dir.clone(),
            Arc::new(HostResolver::new("example.com".to_string())),
            &issue,
            Some(1),
        )
        .await;
        assert!(resolver.has_domain("alice.example.com"));
        assert!(domain_dir(&acme_dir, "alice.example.com").exists());

        // alice opts out → the reconcile drops the cert + dir.
        db.clear_subdomain_enabled(&alice).await.unwrap();
        web_cert_lifecycle_loop(
            db,
            resolver.clone(),
            acme_dir.clone(),
            Arc::new(HostResolver::new("example.com".to_string())),
            &issue,
            Some(1),
        )
        .await;

        assert!(
            !resolver.has_domain("alice.example.com"),
            "an opted-out subdomain must be dropped from the resolver"
        );
        assert!(
            !domain_dir(&acme_dir, "alice.example.com").exists(),
            "the per-subdomain acme dir must be deleted on opt-out"
        );
    }

    // The cert half of the live web apex (`domains-and-tls-bootstrap.md`
    // § Implementation status — web apex live): a domainless-booted box claims
    // a domain post-boot, `apply_primary_identity` swaps the router's
    // `HostResolver` domain, and the SAME loop (same live handle, no restart)
    // starts issuing `<handle>.<new-apex>` certs on its next tick.
    #[tokio::test]
    async fn subdomain_issuance_follows_post_boot_domain_claim() {
        let dir = tempfile::tempdir().unwrap();
        let acme_dir = dir.path().to_path_buf();
        let db = db();
        let alice = [0x01u8; 32];
        seed_subdomain_actor(&db, &alice, "alice").await;

        let (resolver, _default_fp) = default_resolver();
        let issue = {
            let acme_dir = acme_dir.clone();
            move |domain: String| {
                let acme_dir = acme_dir.clone();
                async move {
                    write_cert(&acme_dir, &domain, 90);
                    Ok(())
                }
            }
        };

        // Boot: domainless — empty apex, so no subdomain cert reconciles.
        let host_resolver = Arc::new(HostResolver::new(String::new()));
        web_cert_lifecycle_loop(
            db.clone(),
            resolver.clone(),
            acme_dir.clone(),
            host_resolver.clone(),
            &issue,
            Some(1),
        )
        .await;
        assert!(
            resolver.domain_names().is_empty(),
            "a domainless box must not issue any subdomain cert"
        );

        // Post-boot claim: `apply_primary_identity` swaps the live apex.
        host_resolver.set_nest_domain("claimed.test");

        // The same loop (same live handle) picks the new apex up on the next
        // tick and issues alice's subdomain cert — no restart.
        web_cert_lifecycle_loop(
            db,
            resolver.clone(),
            acme_dir.clone(),
            host_resolver,
            &issue,
            Some(1),
        )
        .await;
        assert!(
            resolver.has_domain("alice.claimed.test"),
            "a post-boot claim must drive per-subdomain issuance on the next tick"
        );
    }

    #[tokio::test]
    async fn issuance_failure_records_retry_state_and_does_not_install() {
        let dir = tempfile::tempdir().unwrap();
        let acme_dir = dir.path().to_path_buf();
        let db = db();
        seed_active_domain(&db, "alice.com").await;

        let (resolver, _default_fp) = default_resolver();
        let issue = move |_domain: String| async move {
            Err(anyhow::anyhow!(
                "validation failed (DNS not pointing at us yet)"
            ))
        };

        web_cert_lifecycle_loop(
            db,
            resolver.clone(),
            acme_dir.clone(),
            Arc::new(HostResolver::new(String::new())),
            issue,
            Some(1),
        )
        .await;

        assert!(
            !resolver.has_domain("alice.com"),
            "a failed issuance must not install a cert"
        );
        let retry = RetryState::load(&domain_dir(&acme_dir, "alice.com"));
        assert_eq!(
            retry.recent_failures.len(),
            1,
            "a failed attempt must be logged to the per-domain retry state"
        );
    }
}
