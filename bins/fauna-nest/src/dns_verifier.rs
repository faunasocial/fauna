//! Public-recursive DNS self-verification for the unified DNS-management
//! surface, per `docs/goal/behavior/dns-management.md` § Live verification
//! (design tracked internally).
//!
//! The nest resolves each expected record against a **public recursive**
//! resolver (system/root, NOT the DNS-provider API and NOT a client's stub —
//! the answer must reflect what peer MTAs and the public Internet actually see)
//! and reports `ok | missing | mismatch | checking` per record. This is the
//! single real resolver the nest grows; it also backs (and replaces) the old
//! always-`false` `web_content/domain.rs::check_dns_txt` placeholder.
//!
//! Layering mirrors `fauna_mail::outbound::mta_sts`:
//!   * [`RecordResolver`] — the `dyn`-compatible I/O seam (one `lookup` per
//!     name+type). [`LiveRecordResolver`] is the `hickory-resolver` impl;
//!     [`NullRecordResolver`] (used when a resolver can't be built / in tests)
//!     reports every lookup as transient so the nest answers "checking", never
//!     a false "missing".
//!   * [`DnsVerifier`] — a short-TTL observed-record cache over a
//!     [`RecordResolver`] (the cache doubles as the rate-limiter: at most one
//!     live lookup per `(name, type)` per [`VERIFY_CACHE_TTL_SECS`]). The
//!     observed-vs-expected verdict comes from the pure
//!     `fauna_mail::dns::verify` comparators so publish and verify never diverge
//!     on what "correct" means.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
// hickory 0.26: typed lookups are gone — records come via `Lookup::answers()` and
// the rdata is extracted by matching the `RData` enum (was `rdata.as_ptr()` etc.).
use hickory_resolver::proto::rr::RData;

use fauna_mail::dns::verify::{
    RecordVerifyStatus, compare_addr, compare_mx, compare_ptr, compare_srv, compare_tlsa,
    compare_txt,
};
use fauna_mail::outbound::mta_sts::ClockFn;

/// What public DNS returned for one `(name, record_type)` lookup. `Records`
/// carries observed values in the canonical string form the pure comparators
/// expect (TXT: joined character-strings; MX: `"<pref> <host>"`; A/AAAA: the IP
/// literal). `Empty` = the name resolves but has no record of that type
/// (NXDOMAIN / empty RRset) → `missing`. `Transient` = the lookup failed in a
/// retryable way (timeout, network, resolver-build failure) → `checking`, and
/// is deliberately NOT cached so the next call retries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LookupOutcome {
    Records(Vec<String>),
    Empty,
    Transient,
}

/// The `dyn`-compatible DNS-lookup seam so tests can substitute a scripted
/// resolver and production holds an `Arc<dyn RecordResolver>`.
#[async_trait]
pub trait RecordResolver: Send + Sync {
    /// Resolve `record_type` (`"TXT" | "MX" | "SRV" | "A" | "AAAA" | "PTR"`,
    /// case-insensitive) at `name`. Never errors out of band — a failed lookup
    /// is surfaced as [`LookupOutcome::Transient`].
    async fn lookup(&self, name: &str, record_type: &str) -> LookupOutcome;
}

/// Resolver that reports every lookup as [`LookupOutcome::Transient`]. Installed
/// when a live resolver can't be built and in `AppState::for_test`, so the
/// verify surface answers "checking" (honest: we couldn't look) rather than a
/// misleading "missing".
pub struct NullRecordResolver;

#[async_trait]
impl RecordResolver for NullRecordResolver {
    async fn lookup(&self, _name: &str, _record_type: &str) -> LookupOutcome {
        LookupOutcome::Transient
    }
}

/// Live [`RecordResolver`] over `hickory-resolver` against the system/public
/// recursive resolver. Native-only — `hickory-resolver` doesn't cross-compile
/// to wasm32, so this never enters the client FFI surface.
pub struct LiveRecordResolver {
    inner: hickory_resolver::TokioResolver,
}

impl LiveRecordResolver {
    pub fn new() -> anyhow::Result<Self> {
        let inner = fauna_mail::outbound::build_hickory_resolver()?;
        Ok(Self { inner })
    }
}

/// Distinguish a genuine "no such record" (NXDOMAIN / empty RRset → `Empty`)
/// from a retryable lookup failure (`Transient`), mirroring the
/// `outbound/dane.rs` NXDOMAIN check.
fn outcome_for_lookup_err(err: &hickory_resolver::net::NetError) -> LookupOutcome {
    // hickory 0.26: `ResolveError`/`ProtoErrorKind::NoRecordsFound` collapsed into
    // `NetError` with an `is_no_records_found()` predicate (covers NXDOMAIN + NoData).
    if err.is_no_records_found() {
        return LookupOutcome::Empty;
    }
    LookupOutcome::Transient
}

/// Render a resolved TLSA RR into the `<usage> <selector> <matching> <hex>`
/// presentation form (RFC 7672 §3) `compare_tlsa` parses — via the same
/// `fauna_mail::outbound::dane::tlsa_presentation_string` the
/// `fauna_mail::dns::host::build_mail_tlsa_record` builder's expected side
/// renders through, so observed and expected are byte-for-byte guaranteed to
/// line up rather than relying on two hand-copies staying in sync.
fn format_tlsa_observed(tlsa: &hickory_resolver::proto::rr::rdata::TLSA) -> String {
    fauna_mail::outbound::dane::tlsa_presentation_string(
        u8::from(tlsa.cert_usage),
        u8::from(tlsa.selector),
        u8::from(tlsa.matching),
        &tlsa.cert_data,
    )
}

#[async_trait]
impl RecordResolver for LiveRecordResolver {
    async fn lookup(&self, name: &str, record_type: &str) -> LookupOutcome {
        match record_type.to_ascii_uppercase().as_str() {
            "TXT" => match self.inner.txt_lookup(name).await {
                Ok(resp) => {
                    let mut out = Vec::new();
                    for rec in resp.answers() {
                        let RData::TXT(txt) = &rec.data else {
                            continue;
                        };
                        let mut joined = String::new();
                        for seg in txt.txt_data.iter() {
                            joined.push_str(&String::from_utf8_lossy(seg));
                        }
                        out.push(joined);
                    }
                    if out.is_empty() {
                        LookupOutcome::Empty
                    } else {
                        LookupOutcome::Records(out)
                    }
                }
                Err(e) => outcome_for_lookup_err(&e),
            },
            "MX" => match self.inner.mx_lookup(name).await {
                Ok(resp) => {
                    let out: Vec<String> = resp
                        .answers()
                        .iter()
                        .filter_map(|rec| match &rec.data {
                            RData::MX(mx) => Some(format!("{} {}", mx.preference, mx.exchange)),
                            _ => None,
                        })
                        .collect();
                    if out.is_empty() {
                        LookupOutcome::Empty
                    } else {
                        LookupOutcome::Records(out)
                    }
                }
                Err(e) => outcome_for_lookup_err(&e),
            },
            // `name` is the service owner (`_caldavs._tcp.<domain>`). Observed
            // values are framed `<priority> <weight> <port> <target>` to match
            // `compare_srv`; the target `Name` Display-formats with a trailing
            // dot, which `compare_srv` normalizes away.
            "SRV" => match self.inner.srv_lookup(name).await {
                Ok(resp) => {
                    let out: Vec<String> = resp
                        .answers()
                        .iter()
                        .filter_map(|rec| match &rec.data {
                            RData::SRV(srv) => Some(format!(
                                "{} {} {} {}",
                                srv.priority, srv.weight, srv.port, srv.target
                            )),
                            _ => None,
                        })
                        .collect();
                    if out.is_empty() {
                        LookupOutcome::Empty
                    } else {
                        LookupOutcome::Records(out)
                    }
                }
                Err(e) => outcome_for_lookup_err(&e),
            },
            "A" => match self.inner.ipv4_lookup(name).await {
                Ok(resp) => {
                    let out: Vec<String> = resp
                        .answers()
                        .iter()
                        .filter_map(|rec| match &rec.data {
                            RData::A(a) => Some(a.0.to_string()),
                            _ => None,
                        })
                        .collect();
                    if out.is_empty() {
                        LookupOutcome::Empty
                    } else {
                        LookupOutcome::Records(out)
                    }
                }
                Err(e) => outcome_for_lookup_err(&e),
            },
            "AAAA" => match self.inner.ipv6_lookup(name).await {
                Ok(resp) => {
                    let out: Vec<String> = resp
                        .answers()
                        .iter()
                        .filter_map(|rec| match &rec.data {
                            RData::AAAA(aaaa) => Some(aaaa.0.to_string()),
                            _ => None,
                        })
                        .collect();
                    if out.is_empty() {
                        LookupOutcome::Empty
                    } else {
                        LookupOutcome::Records(out)
                    }
                }
                Err(e) => outcome_for_lookup_err(&e),
            },
            // `name` is the reverse-pointer owner (`<…>.in-addr.arpa` /
            // `<…>.ip6.arpa`) the host builder produced; resolve the PTR RRset
            // there. Observed values are the target FQDNs, compared name-wise by
            // `compare_ptr`.
            "PTR" => match self
                .inner
                .lookup(name, hickory_resolver::proto::rr::RecordType::PTR)
                .await
            {
                Ok(resp) => {
                    let out: Vec<String> = resp
                        .answers()
                        .iter()
                        .filter_map(|rec| match &rec.data {
                            RData::PTR(ptr) => Some(ptr.to_string()),
                            _ => None,
                        })
                        .collect();
                    if out.is_empty() {
                        LookupOutcome::Empty
                    } else {
                        LookupOutcome::Records(out)
                    }
                }
                Err(e) => outcome_for_lookup_err(&e),
            },
            // `name` is `_25._tcp.mail.<primary>`; resolve the TLSA RRset and
            // render each as a `<usage> <selector> <matching> <hex>` string for
            // `compare_tlsa` (verification of what we publish — DNSSEC-proof is
            // the sending MTA's concern, not ours here).
            "TLSA" => match self
                .inner
                .lookup(name, hickory_resolver::proto::rr::RecordType::TLSA)
                .await
            {
                Ok(resp) => {
                    let out: Vec<String> = resp
                        .answers()
                        .iter()
                        .filter_map(|rec| match &rec.data {
                            RData::TLSA(tlsa) => Some(format_tlsa_observed(tlsa)),
                            _ => None,
                        })
                        .collect();
                    if out.is_empty() {
                        LookupOutcome::Empty
                    } else {
                        LookupOutcome::Records(out)
                    }
                }
                Err(e) => outcome_for_lookup_err(&e),
            },
            other => {
                // Only the matrix's record types are ever requested; an unknown
                // type is a wiring bug. Answer "checking" (never a false
                // "missing") and leave a breadcrumb.
                tracing::warn!("dns_verifier: unsupported record type {other:?} for {name}");
                LookupOutcome::Transient
            }
        }
    }
}

/// Short-TTL observed-record cache. Long enough to absorb the page-load +
/// re-check + background-cadence lookup pressure on the same record, short
/// enough that a freshly-published record goes green quickly. Doubles as the
/// per-record rate limit (design § Verification: "cache results with a short
/// TTL; rate-limit lookups").
pub const VERIFY_CACHE_TTL_SECS: i64 = 60;

#[derive(Clone)]
struct CacheEntry {
    /// Only `Records`/`Empty` are cached; `Transient` is never stored so the
    /// next call retries.
    outcome: LookupOutcome,
    fetched_at: i64,
}

/// Caching verifier over a [`RecordResolver`]. `verify` resolves (or serves a
/// fresh cache hit for) one record and returns the observed values + the
/// observed-vs-expected verdict from the pure `fauna_mail::dns::verify`
/// comparators.
pub struct DnsVerifier {
    resolver: Arc<dyn RecordResolver>,
    clock: ClockFn,
    cache: Mutex<HashMap<(String, String), CacheEntry>>,
}

/// One record's verification result: the observed values + the verdict.
pub struct VerifyResult {
    pub observed: Vec<String>,
    pub status: RecordVerifyStatus,
}

impl DnsVerifier {
    pub fn new(resolver: Arc<dyn RecordResolver>, clock: ClockFn) -> Self {
        Self {
            resolver,
            clock,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Convenience constructor for the "no resolver" path (resolver build
    /// failed, or tests): every lookup is transient → `checking`.
    pub fn null(clock: ClockFn) -> Self {
        Self::new(Arc::new(NullRecordResolver), clock)
    }

    /// The underlying resolver, for raw uncached lookups (the deliverability
    /// diagnostic + blocklist self-check want fresh DNS, not the verify cache).
    pub fn resolver(&self) -> Arc<dyn RecordResolver> {
        self.resolver.clone()
    }

    fn now(&self) -> i64 {
        (self.clock)()
    }

    fn cached(&self, key: &(String, String), now: i64) -> Option<LookupOutcome> {
        let cache = self.cache.lock().ok()?;
        let entry = cache.get(key)?;
        if now.saturating_sub(entry.fetched_at) < VERIFY_CACHE_TTL_SECS {
            Some(entry.outcome.clone())
        } else {
            None
        }
    }

    fn store(&self, key: (String, String), outcome: LookupOutcome, now: i64) {
        // Don't cache transient failures — let the next call retry.
        if matches!(outcome, LookupOutcome::Transient) {
            return;
        }
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(
                key,
                CacheEntry {
                    outcome,
                    fetched_at: now,
                },
            );
        }
    }

    /// Resolve `record_type` at `name` (cache-first) and compare the observed
    /// values against `expected_wire` (the zone-file body `list_records`
    /// reports). `record_type` selects the pure comparator.
    pub async fn verify(&self, name: &str, record_type: &str, expected_wire: &str) -> VerifyResult {
        let now = self.now();
        let key = (name.to_string(), record_type.to_ascii_uppercase());

        let outcome = match self.cached(&key, now) {
            Some(o) => o,
            None => {
                let fresh = self.resolver.lookup(name, record_type).await;
                self.store(key, fresh.clone(), now);
                fresh
            }
        };

        match outcome {
            LookupOutcome::Transient => VerifyResult {
                observed: Vec::new(),
                status: RecordVerifyStatus::Checking,
            },
            LookupOutcome::Empty => VerifyResult {
                observed: Vec::new(),
                status: RecordVerifyStatus::Missing,
            },
            LookupOutcome::Records(observed) => {
                let status = match record_type.to_ascii_uppercase().as_str() {
                    "MX" => compare_mx(expected_wire, &observed),
                    "SRV" => compare_srv(expected_wire, &observed),
                    "A" | "AAAA" => compare_addr(expected_wire, &observed),
                    // PTR: the expected body is the target FQDN; match by name.
                    "PTR" => compare_ptr(expected_wire, &observed),
                    // TLSA: the floor-MX DANE pin; match the `<u> <s> <m> <hex>`
                    // tuple (hex case-insensitive).
                    "TLSA" => compare_tlsa(expected_wire, &observed),
                    // TXT (and any other body framed as a quoted character
                    // string) compares by content equality.
                    _ => compare_txt(expected_wire, &observed),
                };
                VerifyResult { observed, status }
            }
        }
    }

    /// Resolve a TXT record and report whether any observed value equals
    /// `expected` (used by the web-content `_fauna-verify.<domain>` token
    /// check, which compares against an unquoted token rather than a zone-file
    /// body). `false` while the lookup is transient — the caller retries.
    pub async fn txt_contains(&self, name: &str, expected: &str) -> bool {
        let now = self.now();
        let key = (name.to_string(), "TXT".to_string());
        let outcome = match self.cached(&key, now) {
            Some(o) => o,
            None => {
                let fresh = self.resolver.lookup(name, "TXT").await;
                self.store(key, fresh.clone(), now);
                fresh
            }
        };
        matches!(outcome, LookupOutcome::Records(ref obs) if obs.iter().any(|o| o == expected))
    }
}

/// Shared [`RecordResolver`] test double — a scripted resolver answering a
/// programmed outcome per `(name, TYPE)` and counting calls so a cache/rate-
/// limit can be asserted. `pub(crate)` (not nested in `mod tests`) so every
/// module testing against [`RecordResolver`] shares one impl instead of
/// hand-rolling its own (`dns_verifier`'s own tests and
/// `web_content::domain`'s both did, byte-identical bar the call counter,
/// until this lift — found by the same-crate arm of the dev-fleet near-
/// duplicate-function scanner).
#[cfg(test)]
pub(crate) mod test_support {
    use super::{HashMap, LookupOutcome, RecordResolver};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub(crate) struct MockResolver {
        answers: HashMap<(String, String), LookupOutcome>,
        calls: AtomicUsize,
    }

    impl MockResolver {
        pub(crate) fn new(answers: &[((&str, &str), LookupOutcome)]) -> Self {
            Self {
                answers: answers
                    .iter()
                    .map(|((n, t), o)| ((n.to_string(), t.to_uppercase()), o.clone()))
                    .collect(),
                calls: AtomicUsize::new(0),
            }
        }

        /// Total lookups served — read back by callers asserting the cache
        /// suppressed a repeat lookup.
        pub(crate) fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl RecordResolver for MockResolver {
        async fn lookup(&self, name: &str, record_type: &str) -> LookupOutcome {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.answers
                .get(&(name.to_string(), record_type.to_uppercase()))
                .cloned()
                .unwrap_or(LookupOutcome::Empty)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_support::MockResolver;

    fn fixed_clock(t: i64) -> ClockFn {
        Arc::new(move || t)
    }

    #[tokio::test]
    async fn verify_maps_records_to_ok_missing_mismatch() {
        let resolver = Arc::new(MockResolver::new(&[
            (
                ("example.com", "MX"),
                LookupOutcome::Records(vec!["10 mail.example.com.".to_string()]),
            ),
            (
                ("example.com", "TXT"),
                LookupOutcome::Records(vec!["v=spf1 -all".to_string()]),
            ),
            (("_dmarc.example.com", "TXT"), LookupOutcome::Empty),
        ]));
        let v = DnsVerifier::new(resolver, fixed_clock(1000));

        let mx = v.verify("example.com", "MX", "10 mail.example.com").await;
        assert_eq!(mx.status, RecordVerifyStatus::Ok);
        assert_eq!(mx.observed, vec!["10 mail.example.com.".to_string()]);

        let spf = v.verify("example.com", "TXT", "\"v=spf1 mx ~all\"").await;
        assert_eq!(spf.status, RecordVerifyStatus::Mismatch);

        let dmarc = v
            .verify("_dmarc.example.com", "TXT", "\"v=DMARC1; p=reject\"")
            .await;
        assert_eq!(dmarc.status, RecordVerifyStatus::Missing);
        assert!(dmarc.observed.is_empty());
    }

    #[tokio::test]
    async fn verify_dispatches_srv_to_compare_srv() {
        let resolver = Arc::new(MockResolver::new(&[
            (
                ("_caldavs._tcp.example.com", "SRV"),
                LookupOutcome::Records(vec!["0 1 443 mail.example.com.".to_string()]),
            ),
            (
                ("_caldavs._tcp.other.test", "SRV"),
                LookupOutcome::Records(vec!["0 1 8443 mail.other.test.".to_string()]),
            ),
        ]));
        let v = DnsVerifier::new(resolver, fixed_clock(1000));

        let ok = v
            .verify(
                "_caldavs._tcp.example.com",
                "SRV",
                "0 1 443 mail.example.com",
            )
            .await;
        assert_eq!(ok.status, RecordVerifyStatus::Ok);
        assert_eq!(ok.observed, vec!["0 1 443 mail.example.com.".to_string()]);

        // Wrong port served → mismatch (proves SRV-specific comparison, not TXT).
        let bad = v
            .verify("_caldavs._tcp.other.test", "SRV", "0 1 443 mail.other.test")
            .await;
        assert_eq!(bad.status, RecordVerifyStatus::Mismatch);
    }

    #[tokio::test]
    async fn verify_routes_tlsa_to_the_dane_comparator() {
        // A published TLSA matching our floor pin → Ok; routing through
        // compare_tlsa (tuple match), not compare_txt (which would mismatch on
        // the unquoted-string path).
        let pin = format!("3 1 1 {}", "ab".repeat(32));
        let resolver = Arc::new(MockResolver::new(&[(
            ("_25._tcp.mail.example.com", "TLSA"),
            LookupOutcome::Records(vec![pin.clone()]),
        )]));
        let v = DnsVerifier::new(resolver, fixed_clock(1000));
        let r = v.verify("_25._tcp.mail.example.com", "TLSA", &pin).await;
        assert_eq!(r.status, RecordVerifyStatus::Ok);

        // No TLSA published (MX went trusted / never on floor) → Missing.
        let none = Arc::new(MockResolver::new(&[]));
        let v2 = DnsVerifier::new(none, fixed_clock(1000));
        let r2 = v2.verify("_25._tcp.mail.example.com", "TLSA", &pin).await;
        assert_eq!(r2.status, RecordVerifyStatus::Missing);
    }

    #[tokio::test]
    async fn null_resolver_reports_checking() {
        let v = DnsVerifier::null(fixed_clock(1000));
        let r = v.verify("example.com", "MX", "10 mail.example.com").await;
        assert_eq!(r.status, RecordVerifyStatus::Checking);
        assert!(r.observed.is_empty());
    }

    #[tokio::test]
    async fn cache_serves_repeat_lookups_within_ttl() {
        let resolver = Arc::new(MockResolver::new(&[(
            ("example.com", "MX"),
            LookupOutcome::Records(vec!["10 mail.example.com.".to_string()]),
        )]));
        let calls = resolver.clone();
        let v = DnsVerifier::new(resolver, fixed_clock(1000));

        for _ in 0..5 {
            let r = v.verify("example.com", "MX", "10 mail.example.com").await;
            assert_eq!(r.status, RecordVerifyStatus::Ok);
        }
        // Only the first lookup hit the resolver; the rest were cache hits.
        assert_eq!(calls.call_count(), 1);
    }

    #[tokio::test]
    async fn transient_outcome_is_not_cached() {
        let resolver = Arc::new(MockResolver::new(&[(
            ("example.com", "TXT"),
            LookupOutcome::Transient,
        )]));
        let calls = resolver.clone();
        let v = DnsVerifier::new(resolver, fixed_clock(1000));

        for _ in 0..3 {
            let r = v.verify("example.com", "TXT", "\"x\"").await;
            assert_eq!(r.status, RecordVerifyStatus::Checking);
        }
        // Each call re-hit the resolver — transient results are never cached.
        assert_eq!(calls.call_count(), 3);
    }

    #[tokio::test]
    async fn txt_contains_matches_token_membership() {
        let resolver = Arc::new(MockResolver::new(&[(
            ("_fauna-verify.example.com", "TXT"),
            LookupOutcome::Records(vec![
                "unrelated".to_string(),
                "fauna-verify-abc".to_string(),
            ]),
        )]));
        let v = DnsVerifier::new(resolver, fixed_clock(1000));
        assert!(
            v.txt_contains("_fauna-verify.example.com", "fauna-verify-abc")
                .await
        );
        assert!(
            !v.txt_contains("_fauna-verify.example.com", "fauna-verify-xyz")
                .await
        );
    }
}
