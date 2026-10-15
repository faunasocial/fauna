//! Test-only HTTP endpoint seeding the nest's DNS resolver seam.
//!
//! Gated on `test-hooks` **alone** (like `outbound_clock_test_hook`) so it is
//! present in the standard `cargo build -p fauna-nest --features test-hooks`
//! e2e build that `build_node()` produces. It lets a tier_3 test make a record
//! "published" — seeded into [`crate::dns_verifier::DnsVerifier`], answered
//! ahead of the real resolver — with no real DNS, so the scheduled DKIM flip's
//! publication gate (`mail-multidomain.md` § Rotation → *The flip waits for
//! publication*) is reachable from an e2e.
//!
//! Consumers: `tests/e2e-unified/tests/test_mail_bridge_mta.py::
//! test_scheduled_flip_waits_for_publication`, and the blocklist self-check
//! witnesses in `tests/e2e-unified/tests/api/test_mail_health.py` (a seeded
//! DNSBL listing, or a list that does not answer), and the outbound MX
//! witnesses in `tests/e2e-unified/tests/test_mail_bridge_mta.py` (a domain
//! publishing no MX, a failed MX lookup, a preferred MX that cannot be
//! reached) through [`SeededMxRrsetResolver`]. Production never compiles this
//! module.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde::Deserialize;
use serde_json::json;

use crate::dns_verifier::{DnsVerifier, LookupOutcome};
use crate::routes::AppState;
use fauna_mail::outbound::mx::{MxAnswer, MxRrsetResolver};

#[derive(Deserialize)]
struct RecordBody {
    /// The owner name, e.g. `202610._domainkey.example.test`.
    name: String,
    /// The record type (`TXT`, `MX`, …), case-insensitive.
    record_type: String,
    /// The observed values to answer with; empty removes the seed.
    #[serde(default)]
    values: Vec<String>,
    /// Answer with a transient failure (timeout / SERVFAIL) instead —
    /// `values` is then ignored.
    #[serde(default)]
    transient: bool,
    /// Answer authoritatively that no such record exists (NXDOMAIN / NoData)
    /// instead — `values` is then ignored.
    #[serde(default)]
    empty: bool,
}

/// `POST /api/v1/test/dns/record` — seed (or, with no values, unseed) one
/// `(name, type)` answer, with `transient: true` make it fail transiently, or
/// with `empty: true` make it answer that no such record exists.
/// Returns `{"ok": true}`.
async fn handle_record(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RecordBody>,
) -> impl IntoResponse {
    if body.transient {
        state
            .dns_verifier
            .seed_transient_for_test(&body.name, &body.record_type);
    } else if body.empty {
        state
            .dns_verifier
            .seed_empty_for_test(&body.name, &body.record_type);
    } else {
        state
            .dns_verifier
            .seed_for_test(&body.name, &body.record_type, body.values);
    }
    Json(json!({ "ok": true }))
}

/// Mount the `/api/v1/test/dns/record` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route("/api/v1/test/dns/record", post(handle_record))
}

/// The outbound MX seam of a `test-hooks` build: a domain whose `MX` answer is
/// seeded through `POST /api/v1/test/dns/record` resolves from the seed, every
/// other domain through `inner` (the live DNSSEC resolver). It answers
/// `fauna.bridges.resolve_mx` the way
/// [`fauna_mail::outbound::mx::lookup_mx_secure`] would for the same DNS
/// (`smtp-server.md` § MX resolution + IPv4/IPv6 mixed handling, item 1):
///
/// - seeded records (`"<pref> <exchange>"`) → those hosts, `secure: false` (a
///   seed carries no DNSSEC proof, so the bridge skips DANE). An exchange may
///   be `host:port`, which the bridge dials as given — how a test points an MX
///   at a loopback stub on an ephemeral port;
/// - a seeded authoritative no-MX answer → implicit MX `[(0, domain)]`,
///   `secure: true`;
/// - a seeded transient failure → an error, which the bridge tempfails into
///   the retry curve instead of delivering to the domain's own address.
pub struct SeededMxRrsetResolver {
    seeds: Arc<DnsVerifier>,
    inner: Arc<dyn MxRrsetResolver>,
}

impl SeededMxRrsetResolver {
    pub fn new(seeds: Arc<DnsVerifier>, inner: Arc<dyn MxRrsetResolver>) -> Self {
        Self { seeds, inner }
    }
}

#[async_trait::async_trait]
impl MxRrsetResolver for SeededMxRrsetResolver {
    async fn lookup(&self, domain: &str) -> anyhow::Result<MxAnswer> {
        let name = domain.trim_end_matches('.');
        match self.seeds.seeded_for_test(name, "MX") {
            None => self.inner.lookup(domain).await,
            Some(LookupOutcome::Transient) => Err(anyhow::anyhow!(
                "MX lookup failed: seeded transient failure for {name}"
            )),
            Some(LookupOutcome::Empty) => Ok(MxAnswer {
                hosts: vec![(0, name.to_string())],
                secure: true,
            }),
            Some(LookupOutcome::Records(records)) => {
                let hosts = records
                    .iter()
                    .map(|r| parse_seeded_mx(r))
                    .collect::<anyhow::Result<Vec<_>>>()?;
                Ok(MxAnswer {
                    hosts,
                    secure: false,
                })
            }
        }
    }
}

/// One seeded `"<pref> <exchange>"` MX value as a `(priority, host)` pair.
fn parse_seeded_mx(record: &str) -> anyhow::Result<(u16, String)> {
    let (pref, exchange) = record
        .trim()
        .split_once(char::is_whitespace)
        .ok_or_else(|| anyhow::anyhow!("seeded MX {record:?} is not \"<pref> <exchange>\""))?;
    let pref: u16 = pref
        .parse()
        .map_err(|e| anyhow::anyhow!("seeded MX {record:?}: bad preference: {e}"))?;
    Ok((pref, exchange.trim().trim_end_matches('.').to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dns_verifier::NullRecordResolver;

    fn resolver() -> (Arc<DnsVerifier>, SeededMxRrsetResolver) {
        let seeds = Arc::new(DnsVerifier::new(
            Arc::new(NullRecordResolver),
            Arc::new(|| 0),
        ));
        let r = SeededMxRrsetResolver::new(
            seeds.clone(),
            Arc::new(fauna_mail::outbound::mx::NullMxRrsetResolver),
        );
        (seeds, r)
    }

    #[tokio::test]
    async fn seeded_records_resolve_with_ports_and_no_dnssec_proof() {
        let (seeds, r) = resolver();
        seeds.seed_for_test(
            "next.test",
            "MX",
            vec!["20 127.0.0.1:2526".into(), "10 127.0.0.1:2525.".into()],
        );
        let a = r.lookup("next.test.").await.unwrap();
        assert_eq!(
            a.hosts,
            vec![(20, "127.0.0.1:2526".into()), (10, "127.0.0.1:2525".into())]
        );
        assert!(!a.secure);
    }

    #[tokio::test]
    async fn seeded_no_mx_is_implicit_mx_but_seeded_failure_is_an_error() {
        let (seeds, r) = resolver();
        seeds.seed_empty_for_test("bare.test", "MX");
        let a = r.lookup("bare.test").await.unwrap();
        assert_eq!(a.hosts, vec![(0, "bare.test".into())]);
        assert!(a.secure);

        seeds.seed_transient_for_test("down.test", "MX");
        assert!(r.lookup("down.test").await.is_err());
    }

    #[tokio::test]
    async fn unseeded_domain_falls_through_to_the_inner_resolver() {
        let (_seeds, r) = resolver();
        assert_eq!(r.lookup("other.test").await.unwrap(), MxAnswer::default());
    }
}
