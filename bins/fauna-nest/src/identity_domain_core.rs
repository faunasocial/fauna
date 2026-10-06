//! The deployment's identity domain — a **projection of the primary `mail_domains`
//! row**, which IS the nest's identity (`docs/goal/behavior/dns-management.md`).
//! There is no separate identity store: the primary domain is the single source of
//! truth (set at claim from the admin handle; `FAUNA_DOMAIN` is retired). This
//! module only bridges that async DB row to the **sync** `AppState::handle_domain()`
//! / `web_serving_domain()` accessors via the lock-free `AppState.identity_domain`
//! cache (the `cors_origins` `ArcSwap` pattern — the async NAT-mode `RwLock` is
//! unusable from those sync methods).
//!
//! Two entry points, both consulting the primary row so the cache can never drift
//! from it:
//! - [`resolve_identity_domain`] — boot: read the primary (else the
//!   `config.nest.domain` seed) to initialize the cache in `start_server`.
//! - [`apply_primary_identity`] — after a claim / add-domain registers the primary
//!   (`mail_enable::ensure_mail_domain_registered`, or
//!   `bridge_routing_handlers::add_local_domain_handler` for a client
//!   add-first-domain): swap the cache + self-heal TLS.
//!
//! Local/IP boxes register no domain (`mail.<ip>` is nonsense), so their identity
//! falls back to `localhost` (as before) — the access address is not persisted.
//!
//! Design: `docs/goal/architecture/nest/domains-and-tls-bootstrap.md` § Claim.

use std::sync::Arc;

use crate::routes::AppState;

/// Resolve the deployment's identity domain at boot: the primary `mail_domains`
/// row (the single source of truth) wins; absent, fall back to the
/// `config.nest.domain` seed (empty on a domainless box ⇒ `None`, so a fresh box
/// is identity-less until claimed). Read-error / no-primary degrades to the seed.
/// Called once by `start_server` to initialize `AppState.identity_domain`; a box
/// that boots already-claimed thus serves its domain immediately.
pub async fn resolve_identity_domain(
    db: &crate::db::CacheDb,
    config: &crate::config::NestConfig,
) -> Option<String> {
    match db.lookup_primary_mail_domain().await {
        Ok(Some(d)) => Some(d.domain_name),
        Ok(None) => config.nest.domain.clone().filter(|d| !d.is_empty()),
        Err(e) => {
            tracing::error!(
                "lookup_primary_mail_domain at boot failed: {e:#}; falling back to config seed"
            );
            config.nest.domain.clone().filter(|d| !d.is_empty())
        }
    }
}

/// Point the sync identity cache at a domain that just became the primary, and
/// self-heal its TLS. Called from the two production paths that register a
/// **primary** `mail_domains` row, so every path that sets the primary updates the
/// identity exactly once, from one shared primitive:
/// - `mail_enable::ensure_mail_domain_registered` — claim-time (the wizard handle's
///   `@domain`) and the enable / boot safety net;
/// - `bridge_routing_handlers::add_local_domain_handler` — the admin adding the
///   first domain from a client (Admin→DNS) on a domainless-booted box (that path
///   writes the row directly to keep the client's MTA-STS / catch-all / DKIM picks,
///   so it calls this primitive rather than routing through the helper above).
///
/// `domain` is the freshly-registered primary (always a real,
/// `is_public_dns_name` domain — the caller gates that), already normalized.
/// Effects:
/// 1. **Swap** `AppState.identity_domain` so the sync accessors follow immediately.
/// 2. **Point the web `HostResolver`'s apex domain** at the new primary (when the
///    state is web-serving) so subdomain / web-content routing follows the claim
///    with no restart (`domains-and-tls-bootstrap.md` § Implementation status).
/// 3. **Cert self-heal (no restart):** force-re-synthesize the self-signed floor to
///    cover `<domain>` + `mail.<domain>` + `relay.<domain>` + `pds.<domain>`
///    (`self_signed_cert.rs`'s SAN set; SPKI-stable; never clobbers a trusted
///    cert), and wake the ACME lifecycle task to issue a trusted cert now (it
///    reads the apex per-iteration from `handle_domain()`). This is what lets a
///    domainless-booted box get correct TLS the instant it is claimed.
///
/// Best-effort on the cert half (logged, not fatal — the boot floor keeps serving
/// and the ACME poll self-heals regardless).
pub fn apply_primary_identity(state: &AppState, domain: &str) {
    // 1. Swap the sync cache to mirror the new primary.
    state
        .identity_domain
        .store(Some(Arc::new(domain.to_string())));

    // 2. Keep the web `HostResolver`'s apex domain live too, so a domain claimed
    //    post-boot drives subdomain / web-content routing + the reserved-host
    //    guard with no restart (`domains-and-tls-bootstrap.md` § Implementation
    //    status — web apex live). Absent on a non-web-serving test/boot state.
    if let Some(host_resolver) = state.host_resolver.as_ref() {
        host_resolver.set_nest_domain(domain);
    }

    // 3. Self-heal TLS for the new apex.
    if let Err(e) = crate::self_signed_cert::resynthesize_floor_for_domain(&state.acme_dir, domain)
    {
        tracing::warn!(
            "primary-domain identity: floor re-synth for {domain:?} failed: {e:#} \
             (floor stays as-is; ACME will still self-heal)"
        );
    }
    // Wake the ACME lifecycle task to issue a trusted cert for the new apex now
    // (subject to its failed-validation budget — no rate-limit risk).
    state.acme_retry_notify.notify_one();
    // A relay sidecar standing by (no public name until now) is told to fetch:
    // from this moment the nest has a certificate to hand it.
    state
        .relay_cert_changed
        .send_modify(|generation| *generation += 1);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::apply_primary_identity;
    use crate::web_content::serve::HostResolver;

    #[tokio::test]
    async fn apply_primary_identity_updates_web_host_resolver_domain() {
        // Flow-trace: a client add-first-domain / claim registers the primary →
        // `apply_primary_identity` → the web `HostResolver`'s apex domain follows,
        // so a subdomain of the just-claimed domain routes with no restart
        // (`domains-and-tls-bootstrap.md` § Implementation status — web apex live).
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Arc::new(crate::db::CacheDb::open_in_memory().expect("db"));
        let mut st = crate::routes::AppState::for_test(db);
        st.acme_dir = dir.path().to_path_buf();

        let resolver = Arc::new(HostResolver::new("old.test".to_string()));
        resolver.register_subdomain("bob", [7u8; 32]).await;
        st.host_resolver = Some(resolver.clone());

        // Before the claim the new domain's subdomain doesn't route.
        assert!(resolver.resolve("bob.new.test").await.is_none());

        apply_primary_identity(&st, "new.test");

        // Identity swap + the web resolver both followed the claim.
        assert_eq!(st.handle_domain(), "new.test");
        assert_eq!(resolver.resolve("bob.new.test").await, Some([7u8; 32]));
    }
}
