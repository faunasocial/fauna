//! The **domain-expiry watch** — the nest half of row 76.
//!
//! Owner: `docs/goal/architecture/nest/domains-and-tls-bootstrap.md` § Domain
//! loss → *Detection*. The client half (the critical-alerts feeder that turns
//! this record into a banner) lives in `libs/fauna-client-alert-sweep`; the
//! *decision* both halves share is `fauna_protocol::domain_expiry::evaluate`.
//!
//! # What this does, and the shape the section ratified
//!
//! Nothing watches the deployment's registration today — a lapse announces
//! itself as failures, days or weeks after the last moment renewal was cheap.
//! So: on a slow cadence the nest RDAP-queries its **primary** domain and
//! persists `(expiry, statuses, fetched_at, outcome)`; a User-class read kind
//! serves that record to any authenticated session; the sweep feeder reads it
//! and posts or clears.
//!
//! ## Three things a cold read needs
//!
//! **1. Every fetch goes through the SSRF guard, and that is not paranoia
//! theatre.** The base URL of a TLD's RDAP service comes out of the IANA
//! bootstrap file — a *remote document* naming *remote hosts*. That is a
//! caller-supplied URL in every sense that matters (`ssrf.rs`'s own header: any
//! endpoint dialing a URL it did not compile in), and a bootstrap entry pointing
//! at `169.254.169.254` would otherwise turn the watch into a confused deputy
//! reading cloud IMDS credentials on a daily timer. [`ssrf_safe_https_client`]
//! also disables redirects, which matters here for a second reason: RDAP
//! services redirect constantly (the aggregator `rdap.org` is essentially a
//! redirector), and following one silently would defeat the pinning. We follow
//! at most [`MAX_REDIRECTS`] hops, re-running the guard at every hop.
//!
//! **2. The cadence is daily-class, deliberately far from the cert tick.**
//! Registration state moves on the scale of days, and public RDAP operators
//! deserve politeness — one nest asking about one name once a day is invisible;
//! the 5-minute cert reconcile loop would be 288× that for no gained signal.
//!
//! **3. The three outcomes are the whole contract, and "skip" is the one that is
//! easy to get wrong.** A TLD the bootstrap does not serve (many ccTLDs) and a
//! domainless deployment are **skips**: silent on the banner, never a failure,
//! never a retry-with-alarm — absence of data must not alarm. An RDAP server
//! that is present but erroring is a **failure**: the record says so, the client
//! reads `Unreachable`, and any standing alert deliberately stands, because
//! unreachable is not resolved.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use fauna_protocol::domain_expiry::{DomainExpiryRecord, outcomes, skip_reasons};
use serde::Deserialize;

use crate::routes::AppState;
use crate::ssrf::ssrf_safe_https_client;

/// The IANA RDAP bootstrap registry for DNS (RFC 9224) — which RDAP service
/// serves which TLD.
///
/// Compiled in, bucket-1: this is where IANA publishes the map, the same way a
/// root program's trust list is not a setting. § Detection names it explicitly
/// and says the watch has "no config surface anywhere".
const RDAP_BOOTSTRAP_URL: &str = "https://data.iana.org/rdap/dns.json";

/// The aggregating fallback, used when the bootstrap is unreachable or serves no
/// entry we can use. Named in § Detection alongside the bootstrap.
const RDAP_AGGREGATOR_BASE: &str = "https://rdap.org";

/// How often the watch runs — daily-class (see rule 2 in the module note).
const WATCH_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Per-request timeout. Generous: a slow registry RDAP server is normal, and the
/// only cost of waiting is a background task nobody is blocked on.
const FETCH_TIMEOUT: Duration = Duration::from_secs(20);

/// Cap on a single fetched body. The bootstrap file is ~200 KB today and a
/// domain response is a few KB; this is a sanity bound on a remote party, not a
/// tuning knob.
const MAX_FETCH_BYTES: usize = 4 * 1024 * 1024;

/// How many redirect hops to follow, re-running the SSRF guard at each.
///
/// `rdap.org` answers with a 302 to the authoritative registry service, so
/// *some* redirect following is mandatory for the fallback path to work at all;
/// each hop is a fresh guarded client rather than a reqwest redirect policy,
/// which is what keeps every hop pinned.
const MAX_REDIRECTS: usize = 4;

/// The subset of the IANA bootstrap file we read (RFC 9224 § 4): a list of
/// `[[tlds...], [service base URLs...]]` pairs.
#[derive(Debug, Deserialize)]
struct RdapBootstrap {
    #[serde(default)]
    services: Vec<Vec<Vec<String>>>,
}

/// The subset of an RDAP domain response we read (RFC 9083).
#[derive(Debug, Default, Deserialize)]
struct RdapDomain {
    #[serde(default)]
    status: Vec<String>,
    #[serde(default)]
    events: Vec<RdapEvent>,
}

#[derive(Debug, Deserialize)]
struct RdapEvent {
    #[serde(rename = "eventAction", default)]
    event_action: String,
    #[serde(rename = "eventDate", default)]
    event_date: String,
}

/// The TLD of `domain`, lowercased — the bootstrap's lookup key.
fn tld_of(domain: &str) -> Option<String> {
    domain
        .trim_end_matches('.')
        .rsplit('.')
        .next()
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
}

/// Find the RDAP service base URL for `tld` in a parsed bootstrap file.
///
/// Prefers an `https` base — the SSRF guard refuses anything else, and a few
/// bootstrap entries still list `http` alongside it.
fn service_base_for_tld(bootstrap: &RdapBootstrap, tld: &str) -> Option<String> {
    for service in &bootstrap.services {
        let (tlds, urls) = (service.first()?, service.get(1)?);
        if !tlds.iter().any(|t| t.eq_ignore_ascii_case(tld)) {
            continue;
        }
        if let Some(https) = urls.iter().find(|u| u.starts_with("https://")) {
            return Some(https.clone());
        }
    }
    None
}

/// Pull the `expiration` event's date out of an RDAP domain response, as unix
/// seconds.
///
/// RFC 9083 § 4.5 dates are RFC 3339. A response with no expiration event is
/// legitimate (some registries publish none) and yields `None` rather than an
/// error — the status arm still decides.
fn expiry_from(domain: &RdapDomain) -> Option<i64> {
    domain
        .events
        .iter()
        .find(|e| e.event_action.eq_ignore_ascii_case("expiration"))
        .and_then(|e| chrono::DateTime::parse_from_rfc3339(&e.event_date).ok())
        .map(|d| d.timestamp())
}

/// GET `url` through the SSRF guard, following at most [`MAX_REDIRECTS`] hops
/// with the guard re-run at each one.
async fn guarded_get(url: &str) -> Result<Vec<u8>> {
    let mut current = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        let (client, parsed) = ssrf_safe_https_client(&current, FETCH_TIMEOUT, None)
            .await
            .map_err(|e| anyhow!("refusing to fetch {current}: {e}"))?;
        let response = client
            .get(parsed.clone())
            .header("Accept", "application/rdap+json, application/json")
            .send()
            .await
            .with_context(|| format!("GET {current}"))?;

        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| anyhow!("{current} redirected with no Location"))?;
            // Resolve relative Locations against the URL we just fetched; the
            // next loop turn re-runs the SSRF guard on the result, so a redirect
            // to an internal address is refused exactly like a direct one.
            current = parsed
                .join(location)
                .with_context(|| format!("bad Location {location} from {current}"))?
                .to_string();
            continue;
        }

        let response = response
            .error_for_status()
            .with_context(|| format!("GET {current}"))?;
        return crate::ssrf::read_capped(response, MAX_FETCH_BYTES)
            .await
            .with_context(|| format!("read RDAP body from {current}"));
    }
    Err(anyhow!("{url}: too many redirects"))
}

/// What one fetch attempt concluded, before it becomes a stored record.
enum Attempt {
    Checked {
        expires_at: Option<i64>,
        statuses: Vec<String>,
    },
    /// Nothing to check — carries a [`skip_reasons`] token.
    Skipped(&'static str),
}

/// RDAP-query `domain`, resolving its service through the IANA bootstrap and
/// falling back to the aggregator.
///
/// `Err` is the **failure** outcome (RDAP present but erroring); `Ok(Skipped)`
/// is the third outcome. The distinction is the whole three-outcome contract:
/// only a failure retries-with-the-alert-standing.
async fn fetch_registration(domain: &str) -> Result<Attempt> {
    let Some(tld) = tld_of(domain) else {
        // A "domain" with no dot is not a registrable name — nothing a registry
        // could answer for. Skip rather than fail: no amount of retrying makes
        // it checkable.
        return Ok(Attempt::Skipped(skip_reasons::UNSERVED_TLD));
    };

    // The bootstrap being unreachable must not be fatal — the aggregator exists
    // exactly so one unavailable IANA fetch does not blind the watch.
    let base = match guarded_get(RDAP_BOOTSTRAP_URL).await {
        Ok(body) => match serde_json::from_slice::<RdapBootstrap>(&body) {
            Ok(bootstrap) => {
                let found = service_base_for_tld(&bootstrap, &tld);
                if found.is_none() {
                    // The bootstrap parsed and simply does not serve this TLD —
                    // that is the ratified `rdap-unserved-tld` skip, and it is a
                    // *stable* fact, not a transient one. Returning here rather
                    // than falling through to the aggregator is deliberate: the
                    // aggregator can only redirect to a service the bootstrap
                    // would have named, so trying it would turn a clean skip
                    // into a daily 404 recorded as a failure.
                    tracing::debug!(domain, tld, "domain-expiry: TLD not served by RDAP");
                    return Ok(Attempt::Skipped(skip_reasons::UNSERVED_TLD));
                }
                found
            }
            Err(e) => {
                tracing::warn!(error = %e, "domain-expiry: RDAP bootstrap did not parse");
                None
            }
        },
        Err(e) => {
            tracing::warn!(error = %e, "domain-expiry: RDAP bootstrap unreachable");
            None
        }
    };

    let base = base.unwrap_or_else(|| RDAP_AGGREGATOR_BASE.to_string());
    let url = format!("{}/domain/{}", base.trim_end_matches('/'), domain);
    let body = guarded_get(&url).await?;
    let parsed: RdapDomain = serde_json::from_slice(&body)
        .with_context(|| format!("decode RDAP response from {url}"))?;

    Ok(Attempt::Checked {
        expires_at: expiry_from(&parsed),
        statuses: parsed.status,
    })
}

/// One watch tick: resolve the primary domain, fetch, persist.
///
/// Never returns an error — every outcome is a *record*, which is the point: a
/// failure that vanished into a log would leave the client unable to tell
/// "unreachable" from "healthy", and those must never blur.
pub(crate) async fn watch_once(state: &Arc<AppState>) {
    let now = fauna_core::data::Timestamp::now_secs();

    let domain = match state.db.lookup_primary_mail_domain().await {
        Ok(Some(d)) => d.domain_name,
        Ok(None) => {
            store(
                state,
                DomainExpiryRecord {
                    domain: String::new(),
                    expires_at: None,
                    statuses: vec![],
                    fetched_at: now,
                    outcome: outcomes::SKIPPED.into(),
                    detail: Some(skip_reasons::NO_PRIMARY_DOMAIN.into()),
                    extra: Default::default(),
                },
            )
            .await;
            return;
        }
        Err(e) => {
            tracing::warn!(error = %e, "domain-expiry: could not read the primary domain");
            return;
        }
    };

    let record = match fetch_registration(&domain).await {
        Ok(Attempt::Checked {
            expires_at,
            statuses,
        }) => {
            tracing::debug!(
                domain,
                expires_at,
                statuses = ?statuses,
                "domain-expiry: registration checked"
            );
            DomainExpiryRecord {
                domain,
                expires_at,
                statuses,
                fetched_at: now,
                outcome: outcomes::CHECKED.into(),
                detail: None,
                extra: Default::default(),
            }
        }
        Ok(Attempt::Skipped(reason)) => DomainExpiryRecord {
            domain,
            expires_at: None,
            statuses: vec![],
            fetched_at: now,
            outcome: outcomes::SKIPPED.into(),
            detail: Some(reason.into()),
            extra: Default::default(),
        },
        Err(e) => {
            let why = format!("{e:#}");
            tracing::warn!(domain, error = %why, "domain-expiry: RDAP fetch failed");
            DomainExpiryRecord {
                domain,
                expires_at: None,
                statuses: vec![],
                fetched_at: now,
                outcome: outcomes::FAILED.into(),
                detail: Some(why),
                extra: Default::default(),
            }
        }
    };
    store(state, record).await;
}

async fn store(state: &Arc<AppState>, record: DomainExpiryRecord) {
    if let Err(e) = state.db.put_domain_expiry(&record).await {
        tracing::warn!(error = %e, "domain-expiry: could not persist the record");
    }
}

/// Spawn the periodic watch.
///
/// The first tick is **not** skipped: a box that just booted (or just claimed a
/// domain) should learn where its registration stands now, not tomorrow — and on
/// a domainless deployment the tick reads one row, writes one skip, and makes no
/// network request at all.
pub fn spawn_domain_expiry_watch(state: Arc<AppState>) {
    let scope = state.clone();
    scope.scope_handle(crate::sweeper::spawn_periodic_sweeper(
        WATCH_INTERVAL,
        false,
        move || {
            let state = state.clone();
            async move { watch_once(&state).await }
        },
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bootstrap(json: &str) -> RdapBootstrap {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn tld_extraction_handles_the_root_dot_and_case() {
        assert_eq!(tld_of("example.ORG").as_deref(), Some("org"));
        assert_eq!(tld_of("a.b.example.co.uk").as_deref(), Some("uk"));
        assert_eq!(tld_of("example.org.").as_deref(), Some("org"));
        // Not a registrable name — the caller turns this into a skip.
        assert_eq!(tld_of("localhost"), Some("localhost".into()));
        assert_eq!(tld_of(""), None);
    }

    #[test]
    fn bootstrap_lookup_finds_the_https_base_and_ignores_other_services() {
        let b = bootstrap(
            r#"{"services":[
                [["com","net"],["https://rdap.verisign.example/v1/"]],
                [["org"],["http://insecure.example/","https://rdap.publicinterest.example/"]]
            ]}"#,
        );
        assert_eq!(
            service_base_for_tld(&b, "com").as_deref(),
            Some("https://rdap.verisign.example/v1/")
        );
        // An entry whose first URL is plain http must yield the https sibling —
        // the SSRF guard refuses http outright, so picking urls[0] blindly would
        // turn a perfectly servable TLD into a permanent failure.
        assert_eq!(
            service_base_for_tld(&b, "org").as_deref(),
            Some("https://rdap.publicinterest.example/")
        );
        assert_eq!(service_base_for_tld(&b, "example"), None);
    }

    #[test]
    fn bootstrap_lookup_is_case_insensitive() {
        let b = bootstrap(r#"{"services":[[["COM"],["https://a.example/"]]]}"#);
        assert_eq!(
            service_base_for_tld(&b, "com").as_deref(),
            Some("https://a.example/")
        );
    }

    #[test]
    fn expiry_is_read_from_the_expiration_event_only() {
        let d: RdapDomain = serde_json::from_str(
            r#"{"status":["active"],"events":[
                {"eventAction":"registration","eventDate":"2020-01-01T00:00:00Z"},
                {"eventAction":"expiration","eventDate":"2027-03-04T05:06:07Z"},
                {"eventAction":"last changed","eventDate":"2026-01-01T00:00:00Z"}
            ]}"#,
        )
        .unwrap();
        // 2027-03-04T05:06:07Z. Cross-check, so a future edit to this fixture
        // cannot quietly re-derive the expectation from the code it tests:
        // 2027-01-01 = 1_798_761_600; +59 d (Jan 31 + Feb 28) = 1_803_859_200;
        // +3 d = 1_804_118_400; +5 h 6 m 7 s = 1_804_136_767.
        assert_eq!(expiry_from(&d), Some(1_804_136_767));
    }

    #[test]
    fn a_response_with_no_expiration_event_yields_no_date_rather_than_an_error() {
        let d: RdapDomain =
            serde_json::from_str(r#"{"status":["client hold"],"events":[]}"#).unwrap();
        assert_eq!(expiry_from(&d), None);
        assert_eq!(d.status, vec!["client hold".to_string()]);
    }

    /// RDAP responses carry far more than we read; a strict decode would break
    /// the watch on any registry that adds a field.
    #[test]
    fn unknown_rdap_fields_are_ignored() {
        let d: RdapDomain = serde_json::from_str(
            r#"{"objectClassName":"domain","ldhName":"example.org",
                "status":["active"],"nameservers":[{"ldhName":"a.example"}],
                "events":[{"eventAction":"expiration","eventDate":"2027-01-01T00:00:00Z",
                           "eventActor":"someone"}]}"#,
        )
        .unwrap();
        assert_eq!(d.status, vec!["active".to_string()]);
        assert!(expiry_from(&d).is_some());
    }

    #[test]
    fn a_malformed_expiry_date_does_not_panic_or_fabricate_a_date() {
        let d: RdapDomain = serde_json::from_str(
            r#"{"events":[{"eventAction":"expiration","eventDate":"not a date"}]}"#,
        )
        .unwrap();
        assert_eq!(expiry_from(&d), None);
    }
}
