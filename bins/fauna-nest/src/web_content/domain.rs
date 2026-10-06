//! Custom domain registration and DNS verification for web content hosting.
//!
//! The `fauna.web.domain.{set,get}` WS-RPC handlers (`web_handlers.rs`) call the
//! `register_domain` core below; the HTTP twins were deleted in the orphaned-twin
//! residue sweep (no remaining consumer).
//!
//! A background task (`domain_lifecycle_task`) runs every 5 minutes and does
//! two things per pass: it checks pending domains for a TXT record at
//! `_fauna-verify.{domain}` (advancing "pending" → "verified" → "active"), and
//! it reconciles the live `HostResolver`'s custom-domain routing map against
//! the `active` set — so a domain that goes live, or whose row is deleted,
//! starts/stops being served with no nest restart.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use rand::Rng;

use crate::db::CacheDb;
use crate::web_content::serve::HostResolver;

// ==================== Constants ====================

const MAX_DOMAINS_PER_ACCOUNT: i64 = 3;

// ==================== Domain registration ====================

/// The result of a successful domain registration.
#[derive(Debug)]
pub struct DomainRegistration {
    pub domain: String,
    pub verify_token: String,
}

/// Register a custom domain for an actor.
///
/// Normalizes + validates the user-supplied `domain` (lowercase, strip a
/// trailing dot, reject IP/wildcard/port/path/malformed strings via the shared
/// `fauna_core::web::normalize_custom_domain`), rejects any host this nest
/// already owns (its apex + `mail.`/`mta-sts.`/`app.<apex>` — else an `active`
/// row would shadow the apex actor), enforces the per-account domain limit,
/// rejects duplicates, generates a verification token, and inserts the row with
/// `status = "pending"`. `apex_domain` is the nest's serving domain
/// (`AppState::web_serving_domain`; empty on a domainless box).
///
/// The reserved/self rejection is defense-in-depth *behind* DNS verification (a
/// non-admin cannot publish `_fauna-verify` for the apex), so isolation no longer
/// rests on DNS alone.
pub async fn register_domain(
    db: &CacheDb,
    actor_id: &[u8; 32],
    domain: &str,
    apex_domain: &str,
) -> anyhow::Result<DomainRegistration> {
    // Normalize + validate (shared, WASM-safe) before anything touches the DB.
    let domain = fauna_core::web::normalize_custom_domain(domain)
        .map_err(|e| anyhow::anyhow!("invalid domain: {e}"))?;

    // Never let a custom domain claim a name the nest serves itself.
    if fauna_core::web::is_nest_owned_host(&domain, apex_domain) {
        anyhow::bail!("domain is reserved by this nest");
    }

    // Enforce per-account limit.
    let count = db.count_web_domains_for_actor(actor_id).await?;
    if count >= MAX_DOMAINS_PER_ACCOUNT {
        anyhow::bail!("domain limit reached");
    }

    // Reject duplicate domains (across all actors).
    if db.get_web_domain_by_domain(&domain).await?.is_some() {
        anyhow::bail!("domain already registered");
    }

    // Generate a random verification token.
    let token_suffix: u64 = rand::thread_rng().r#gen();
    let verify_token = format!("fauna-verify-{:016x}", token_suffix);

    db.insert_web_domain(actor_id, &domain, &verify_token)
        .await?;

    Ok(DomainRegistration {
        domain,
        verify_token,
    })
}

// ==================== DNS verification background task ====================

/// Check whether `_fauna-verify.{domain}` serves the expected TXT token.
///
/// Backed by the shared public-recursive [`DnsVerifier`](crate::dns_verifier)
/// — the same resolver the unified DNS-management verify surface uses (the
/// always-`false` placeholder this replaces is gone). Returns `false` while the
/// lookup is transient (resolver unavailable / timeout); the caller retries on
/// the next poll.
async fn check_dns_txt(
    verifier: &crate::dns_verifier::DnsVerifier,
    domain: &str,
    expected_token: &str,
) -> bool {
    verifier
        .txt_contains(&format!("_fauna-verify.{domain}"), expected_token)
        .await
}

/// One verification pass: list all domains, and for each `pending` one whose
/// `_fauna-verify.<domain>` TXT record now serves its token, advance
/// `pending → verified → active`. Returns the number of domains advanced to
/// `active` this pass. Half of [`domain_lifecycle_task`]'s loop body (the other
/// half is [`reconcile_custom_domain_routing_once`]), factored out so it can be
/// unit-tested without spinning the 5-minute interval (mirrors
/// `acme_http01::cert_lifecycle_loop`'s testable seam).
///
/// Issuance of the per-domain ACME cert is a separate concern (the cert
/// lifecycle loop, web-content Slice 3) that picks up `active` domains; this
/// pass only drives DNS-verification status.
pub async fn verify_pending_domains_once(
    db: &CacheDb,
    verifier: &crate::dns_verifier::DnsVerifier,
) -> anyhow::Result<usize> {
    let domains = db.list_all_web_domains().await?;
    let mut advanced = 0usize;
    for row in domains.into_iter().filter(|r| r.status == "pending") {
        if check_dns_txt(verifier, &row.domain, &row.verify_token).await {
            if let Err(e) = db.update_web_domain_status(&row.domain, "verified").await {
                tracing::error!(
                    "verify_pending_domains_once: set verified for {} failed: {e}",
                    row.domain
                );
                continue;
            }
            if let Err(e) = db.update_web_domain_status(&row.domain, "active").await {
                tracing::error!(
                    "verify_pending_domains_once: set active for {} failed: {e}",
                    row.domain
                );
                continue;
            }
            advanced += 1;
        }
    }
    Ok(advanced)
}

/// One routing-reconciliation pass: project the `active` `web_domains` rows to
/// `domain → owner` and make that the live [`HostResolver`]'s whole
/// custom-domain map.
///
/// This is what makes custom-domain routing follow the DB with no restart —
/// the boot seed was the map's only writer, so before this existed a domain that
/// went `active` post-boot resolved to nothing (404 / apex fallthrough) and a
/// deleted one kept serving until the nest restarted
/// (`web-content-hosting.md` § Registration and DNS verification — "all steps
/// after the user sets the two DNS records are automatic").
///
/// Runs right after [`verify_pending_domains_once`] in the same pass, so a
/// domain verified this tick routes this tick. A DB read error propagates and
/// the caller skips the pass **without touching the map** — reconciling from a
/// partial set would deroute every live site (the same guard
/// `cert::reconcile_once` documents for the per-domain cert map).
///
/// Non-`active` rows (`pending`/`verified`) are deliberately absent: a domain
/// that has not finished DNS verification must not be servable.
///
/// Known, bounded race, accepted rather than machined away: a `domain.delete`
/// landing between this pass's DB read and its map write re-adds the deleted
/// name, which the next pass then drops (≤ 5 min). The window is the few
/// microseconds between the two, it self-heals, and it is the same shape
/// `cert::reconcile_once` already carries for the per-domain cert map — a
/// generation counter to close it would cost more clarity than the window is
/// worth.
pub async fn reconcile_custom_domain_routing_once(
    db: &CacheDb,
    resolver: &HostResolver,
) -> anyhow::Result<()> {
    let rows = db.list_all_web_domains().await?;
    let mut desired: HashMap<String, [u8; 32]> = HashMap::new();
    for row in rows.into_iter().filter(|r| r.status == "active") {
        match <[u8; 32]>::try_from(row.actor_id.as_slice()) {
            Ok(actor) => {
                desired.insert(row.domain, actor);
            }
            Err(_) => tracing::warn!(
                "web host resolver: skipping web domain {} with non-32-byte actor_id",
                row.domain
            ),
        }
    }
    resolver.reconcile_custom_domains(desired).await;
    Ok(())
}

/// Background task: every 5 minutes, attempt DNS verification for all pending
/// domains (advancing a matching one "pending" → "verified" → "active"), then
/// reconcile the live `HostResolver`'s custom-domain map against the `active`
/// set so routing follows the table with no restart.
///
/// Spawned from `main.rs` once `AppState` exists, with the shared
/// `Arc<DnsVerifier>` (the same public-recursive resolver the unified
/// DNS-management verify surface uses) and the router's live `HostResolver`
/// (the same handle the per-domain cert lifecycle loop takes).
///
/// The reconcile is the *steady-state* authority; deleting a domain also drops
/// it from the resolver immediately in `fauna.web.domain.delete`, so a
/// deregistered site stops serving at once rather than within a poll interval.
pub async fn domain_lifecycle_task(
    db: Arc<CacheDb>,
    verifier: Arc<crate::dns_verifier::DnsVerifier>,
    host_resolver: Arc<HostResolver>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(300));
    // The first tick fires immediately; skip it so we don't hammer DNS
    // at startup before the server is fully initialised. Routing is already
    // correct at this point — `start_server`'s boot seed ran the same
    // reconcile before spawning us.
    interval.tick().await;

    loop {
        interval.tick().await;

        if let Err(e) = verify_pending_domains_once(&db, &verifier).await {
            tracing::error!("domain_lifecycle_task: verification pass failed: {e}");
        }
        if let Err(e) = reconcile_custom_domain_routing_once(&db, &host_resolver).await {
            tracing::error!("domain_lifecycle_task: routing reconcile failed: {e}");
        }
    }
}

// ==================== Tests ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;
    use crate::dns_verifier::test_support::MockResolver;
    use crate::dns_verifier::{DnsVerifier, LookupOutcome};
    use fauna_mail::outbound::mta_sts::ClockFn;

    fn fixed_clock(t: i64) -> ClockFn {
        Arc::new(move || t)
    }

    fn actor() -> [u8; 32] {
        [0xBBu8; 32]
    }

    fn actor2() -> [u8; 32] {
        [0xCCu8; 32]
    }

    fn db() -> CacheDb {
        CacheDb::open_in_memory().unwrap()
    }

    // ⚠ The nest apex in these tests is `nest.example.org`, NOT `example.com`.
    // Every test here contrasts a user's custom domain (`*.example.com`)
    // against the nest's own apex, and the publish scrub rules rewrite
    // `example.com` -> `example.com`. With the apex on the
    // scrub target the two collapse in the shipped tree, so `register_domain`
    // would be handed the very apex it exists to refuse — turning six passing
    // tests red in the public repo and nowhere else. `transform.py`'s
    // `verify_no_scrub_collisions` gate now fails the dry-run on this.
    // (Found by the local public-CI replay, 2026-08-24.)

    // ---- register_domain_creates_pending_entry ----

    #[tokio::test]
    async fn register_domain_creates_pending_entry() {
        let db = db();
        let actor = actor();

        let reg = register_domain(&db, &actor, "example.com", "nest.example.org")
            .await
            .expect("registration should succeed");

        // Token has the expected prefix.
        assert!(
            reg.verify_token.starts_with("fauna-verify-"),
            "token should start with fauna-verify-, got: {}",
            reg.verify_token
        );
        assert_eq!(reg.domain, "example.com");

        // The row exists in the database with status "pending".
        let row = db
            .get_web_domain_by_domain("example.com")
            .await
            .unwrap()
            .expect("row should exist");
        assert_eq!(row.status, "pending");
        assert_eq!(row.verify_token, reg.verify_token);
        assert!(row.verified_at.is_none());
    }

    // ---- register_domain_enforces_limit ----

    #[tokio::test]
    async fn register_domain_enforces_limit() {
        let db = db();
        let actor = actor();

        // Register exactly MAX_DOMAINS_PER_ACCOUNT (3) domains — all must succeed.
        for i in 0..MAX_DOMAINS_PER_ACCOUNT {
            let domain = format!("domain{}.example.com", i);
            register_domain(&db, &actor, &domain, "nest.example.org")
                .await
                .unwrap_or_else(|e| panic!("domain {i} registration failed: {e}"));
        }

        // The 4th registration must fail.
        let result = register_domain(&db, &actor, "fourth.example.com", "nest.example.org").await;
        assert!(result.is_err(), "4th registration should be rejected");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("domain limit reached"),
            "error should mention limit, got: {err_msg}"
        );

        // Count still equals the limit.
        let count = db.count_web_domains_for_actor(&actor).await.unwrap();
        assert_eq!(count, MAX_DOMAINS_PER_ACCOUNT);
    }

    // ---- register_domain_rejects_duplicate ----

    #[tokio::test]
    async fn register_domain_rejects_duplicate() {
        let db = db();
        let actor = actor();
        let actor2 = actor2();

        // First registration succeeds.
        register_domain(&db, &actor, "shared.example.com", "nest.example.org")
            .await
            .unwrap();

        // Same domain by the same actor must be rejected.
        let same_actor_result =
            register_domain(&db, &actor, "shared.example.com", "nest.example.org").await;
        assert!(
            same_actor_result.is_err(),
            "duplicate by same actor should be rejected"
        );
        assert!(
            same_actor_result
                .unwrap_err()
                .to_string()
                .contains("already registered")
        );

        // Same domain by a different actor must also be rejected.
        let other_actor_result =
            register_domain(&db, &actor2, "shared.example.com", "nest.example.org").await;
        assert!(
            other_actor_result.is_err(),
            "duplicate by different actor should be rejected"
        );
        assert!(
            other_actor_result
                .unwrap_err()
                .to_string()
                .contains("already registered")
        );
    }

    // ---- register_domain_rejects_nest_owned_hosts ----

    #[tokio::test]
    async fn register_domain_rejects_nest_owned_hosts() {
        let db = db();
        let actor = actor();
        // The apex itself and every reserved sub-host are refused — they would
        // shadow the apex actor or a nest service host.
        for host in [
            "nest.example.org",
            "app.nest.example.org",
            "mail.nest.example.org",
            "mta-sts.nest.example.org",
            // case-insensitively too (normalize lowercases first)
            "App.Nest.Example.ORG",
        ] {
            let err = register_domain(&db, &actor, host, "nest.example.org")
                .await
                .expect_err("nest-owned host must be rejected");
            assert!(
                err.to_string().contains("reserved by this nest"),
                "{host} should be rejected as nest-owned, got: {err}"
            );
        }
        // Nothing was persisted.
        assert_eq!(db.count_web_domains_for_actor(&actor).await.unwrap(), 0);
    }

    // ---- register_domain_rejects_malformed ----

    #[tokio::test]
    async fn register_domain_rejects_malformed() {
        let db = db();
        let actor = actor();
        for bad in [
            "192.168.1.1",
            "*.example.com",
            "https://example.com",
            "example.com:8443",
            "example.com/path",
            "localhost",
            "",
        ] {
            let err = register_domain(&db, &actor, bad, "nest.example.org")
                .await
                .expect_err("malformed domain must be rejected");
            assert!(
                err.to_string().contains("invalid domain"),
                "{bad} should be rejected as invalid, got: {err}"
            );
        }
        assert_eq!(db.count_web_domains_for_actor(&actor).await.unwrap(), 0);
    }

    // ---- register_domain_normalizes_before_store ----

    #[tokio::test]
    async fn register_domain_normalizes_before_store() {
        let db = db();
        let actor = actor();
        let reg = register_domain(&db, &actor, "  Blog.Example.COM.  ", "nest.example.org")
            .await
            .expect("registration should succeed");
        assert_eq!(
            reg.domain, "blog.example.com",
            "stored domain is normalized"
        );
        // The normalized form is what's persisted + deduped against.
        assert!(
            db.get_web_domain_by_domain("blog.example.com")
                .await
                .unwrap()
                .is_some()
        );
        let dup = register_domain(&db, &actor, "BLOG.EXAMPLE.COM", "nest.example.org").await;
        assert!(
            dup.unwrap_err().to_string().contains("already registered"),
            "a case-variant of an existing domain is a duplicate"
        );
    }

    // ---- verify_pending_domains_once_advances_on_matching_txt ----

    #[tokio::test]
    async fn verify_pending_domains_once_advances_on_matching_txt() {
        let db = db();
        let actor = actor();

        let reg = register_domain(&db, &actor, "example.com", "nest.example.org")
            .await
            .expect("registration should succeed");

        // The verifier sees the token published at `_fauna-verify.example.com`.
        let resolver = Arc::new(MockResolver::new(&[(
            ("_fauna-verify.example.com", "TXT"),
            LookupOutcome::Records(vec![reg.verify_token.clone()]),
        )]));
        let verifier = DnsVerifier::new(resolver, fixed_clock(1000));

        let advanced = verify_pending_domains_once(&db, &verifier)
            .await
            .expect("pass succeeds");
        assert_eq!(advanced, 1, "the matching domain should advance");

        let row = db
            .get_web_domain_by_domain("example.com")
            .await
            .unwrap()
            .expect("row exists");
        assert_eq!(row.status, "active");
        assert!(row.verified_at.is_some(), "verified_at recorded");

        // A second pass is a no-op (no longer pending).
        let again = verify_pending_domains_once(&db, &verifier)
            .await
            .expect("pass succeeds");
        assert_eq!(again, 0);
    }

    // ---- verify_pending_domains_once_holds_when_txt_absent ----

    #[tokio::test]
    async fn verify_pending_domains_once_holds_when_txt_absent() {
        let db = db();
        let actor = actor();

        register_domain(&db, &actor, "example.com", "nest.example.org")
            .await
            .expect("registration should succeed");

        // Resolver returns the wrong token → no match.
        let resolver = Arc::new(MockResolver::new(&[(
            ("_fauna-verify.example.com", "TXT"),
            LookupOutcome::Records(vec!["fauna-verify-deadbeefdeadbeef".to_string()]),
        )]));
        let verifier = DnsVerifier::new(resolver, fixed_clock(1000));

        let advanced = verify_pending_domains_once(&db, &verifier)
            .await
            .expect("pass succeeds");
        assert_eq!(advanced, 0, "mismatched token must not advance");

        let row = db
            .get_web_domain_by_domain("example.com")
            .await
            .unwrap()
            .expect("row exists");
        assert_eq!(row.status, "pending", "stays pending until token matches");
    }

    // ---- routing reconcile (the live-HostResolver half) ----

    /// A domain that finishes DNS verification **while the nest is running**
    /// becomes servable in the same pass — no restart.
    ///
    /// This is the link the boot seed alone left broken: `resolve()` is the
    /// exact seam `web_content_or_info` calls to turn a `Host` header into an
    /// actor, so a `None` here is the 404/apex-fallthrough the user saw despite
    /// a valid cert. (Serving from a resolved actor is separately pinned by
    /// `serve::tests::serve_web_content_returns_file`.)
    #[tokio::test]
    async fn a_domain_verified_while_running_starts_routing_without_a_restart() {
        let db = db();
        let actor = actor();
        let resolver = HostResolver::new("nest.example.org".to_string());

        let reg = register_domain(&db, &actor, "example.com", "nest.example.org")
            .await
            .expect("registration should succeed");

        // Before verification the domain is `pending` — never servable.
        reconcile_custom_domain_routing_once(&db, &resolver)
            .await
            .expect("reconcile succeeds");
        assert_eq!(
            resolver.resolve("example.com").await,
            None,
            "a pending domain must not be routed"
        );

        // The user publishes the TXT record; one lifecycle pass advances it.
        let dns = Arc::new(MockResolver::new(&[(
            ("_fauna-verify.example.com", "TXT"),
            LookupOutcome::Records(vec![reg.verify_token.clone()]),
        )]));
        let verifier = DnsVerifier::new(dns, fixed_clock(1000));
        assert_eq!(
            verify_pending_domains_once(&db, &verifier).await.unwrap(),
            1
        );
        reconcile_custom_domain_routing_once(&db, &resolver)
            .await
            .expect("reconcile succeeds");

        assert_eq!(
            resolver.resolve("example.com").await,
            Some(actor),
            "a newly-active domain must route to its owner without a restart"
        );
    }

    /// The reconcile is a projection of the `active` set, so a domain whose row
    /// is gone stops routing — the backstop behind `domain.delete`'s immediate
    /// removal (and the only mechanism for a row that leaves `active` by any
    /// other path).
    #[tokio::test]
    async fn a_deleted_domain_stops_routing_on_the_next_pass() {
        let db = db();
        let actor = actor();
        let resolver = HostResolver::new("nest.example.org".to_string());

        register_domain(&db, &actor, "example.com", "nest.example.org")
            .await
            .unwrap();
        db.update_web_domain_status("example.com", "active")
            .await
            .unwrap();
        reconcile_custom_domain_routing_once(&db, &resolver)
            .await
            .unwrap();
        assert_eq!(resolver.resolve("example.com").await, Some(actor));

        db.delete_web_domain("example.com").await.unwrap();
        reconcile_custom_domain_routing_once(&db, &resolver)
            .await
            .unwrap();

        assert_eq!(
            resolver.resolve("example.com").await,
            None,
            "a deregistered domain must stop routing"
        );
    }

    /// Replace-all must not disturb the resolver's other maps — the apex
    /// catch-all and opted-in subdomains are separate projections with their
    /// own authorities (`web-content-hosting.md` § Routing).
    #[tokio::test]
    async fn the_reconcile_leaves_apex_and_subdomain_routing_alone() {
        let db = db();
        let actor = actor();
        let other = actor2();
        let resolver = HostResolver::new("nest.example.org".to_string());
        resolver.set_apex_actor(Some(other)).await;
        resolver.register_subdomain("alice", other).await;

        register_domain(&db, &actor, "example.com", "nest.example.org")
            .await
            .unwrap();
        db.update_web_domain_status("example.com", "active")
            .await
            .unwrap();
        reconcile_custom_domain_routing_once(&db, &resolver)
            .await
            .unwrap();

        assert_eq!(resolver.resolve("example.com").await, Some(actor));
        assert_eq!(
            resolver.resolve("alice.nest.example.org").await,
            Some(other),
            "subdomain routing survives a custom-domain reconcile"
        );
        assert_eq!(
            resolver.resolve("nest.example.org").await,
            Some(other),
            "the apex catch-all survives a custom-domain reconcile"
        );
    }
}
