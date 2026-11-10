//! MX resolution + RFC 8305 Happy Eyeballs.
//!
//! Implements docs/goal/behavior/smtp-server.md § MX resolution +
//! IPv4/IPv6 mixed handling:
//!   * Resolve MX records, sort by priority ascending.
//!   * Same-priority cluster is round-robined per RFC 5321 §5.1.
//!   * Implicit MX (no MX RRset) uses A/AAAA at priority 0 per
//!     RFC 5321 §5.
//!   * Per host, resolve A + AAAA. Connect via Happy Eyeballs with a
//!     250 ms IPv6 preference delay per RFC 8305.
//!   * Per-host timeouts: 30 s connect, 60 s per command, 600 s total
//!     (constants exported for the caller).
//!
//! This module owns the pure-Rust resolution + ordering. The
//! `MxResolver` trait is mockable so unit tests don't hit DNS. The
//! `happy_eyeballs_connect` function is currently a stub that lands as
//! a follow-up sub-task: the wire-level race requires `tokio::net` +
//! `tokio::time` and is exercised by the live bridge, not unit tests.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use async_trait::async_trait;
use rand::seq::SliceRandom;
#[cfg(feature = "outbound-net")]
use std::sync::Arc;

#[cfg(feature = "outbound-net")]
use crate::outbound::dnssec_path::{DnssecPath, ResolverPath};

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
pub const PER_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
pub const TOTAL_TIMEOUT: Duration = Duration::from_secs(600);
pub const HAPPY_EYEBALLS_PREFER_V6_DELAY: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MxHost {
    pub priority: u16,
    pub hostname: String,
    pub addrs: Vec<IpAddr>,
}

#[async_trait]
pub trait MxResolver: Send + Sync {
    async fn lookup_mx(&self, domain: &str) -> anyhow::Result<Vec<(u16, String)>>;
    async fn lookup_a(&self, host: &str) -> anyhow::Result<Vec<Ipv4Addr>>;
    async fn lookup_aaaa(&self, host: &str) -> anyhow::Result<Vec<Ipv6Addr>>;
}

/// Resolve MX records for `domain`, sort by priority, round-robin within
/// same-priority clusters, then fill in A/AAAA per host. `ipv6_enabled`
/// gates AAAA lookups for IPv4-only-egress deployments.
///
/// Hosts whose A+AAAA resolution comes back empty are dropped (no
/// addresses to connect to).
///
/// # A failed lookup is an error, not an implicit MX
///
/// The implicit-MX rule (RFC 5321 §5.1; `smtp-server.md` § MX resolution,
/// *"Implicit MX (**no MX RR**, A/AAAA only)"*) fires on the resolver
/// authoritatively answering **"this domain publishes no MX records"** — an
/// `Ok(vec![])` here. A lookup that *failed* — SERVFAIL, timeout, no route —
/// says nothing about what the domain publishes, so it propagates and the
/// caller tempfails into the retry curve.
///
/// This is not pedantry about error handling: collapsing the two would deliver
/// mail to the recipient domain's own A record whenever DNS hiccuped, and for
/// any domain whose real MX is a third party that is a host which was never
/// meant to receive its mail. It is also the rule the Go MTA's production
/// `LiveMXResolver` already implements — implicit MX only on
/// `dnsErr.IsNotFound`, every other error returned to the caller
/// (`bins/fauna-bridges/internal/mta/outbound.go`) — so a resolver written
/// against this trait should map "no records" to `Ok(vec![])` and everything
/// else to `Err`, exactly as that one does.
pub async fn resolve_mx(
    domain: &str,
    ipv6_enabled: bool,
    resolver: &dyn MxResolver,
) -> anyhow::Result<Vec<MxHost>> {
    let raw = resolver.lookup_mx(domain).await?;
    let mx_records: Vec<(u16, String)> = if raw.is_empty() {
        // Implicit MX — RFC 5321 §5: use the domain itself at priority 0.
        vec![(0, domain.to_string())]
    } else {
        raw
    };

    // Group by priority, round-robin within each group, then concat in
    // ascending priority order.
    let mut by_prio: std::collections::BTreeMap<u16, Vec<String>> = Default::default();
    for (prio, host) in mx_records {
        by_prio.entry(prio).or_default().push(host);
    }
    let mut rng = rand::thread_rng();
    let mut ordered_hosts: Vec<(u16, String)> = Vec::new();
    for (prio, mut group) in by_prio {
        group.shuffle(&mut rng);
        for host in group {
            ordered_hosts.push((prio, host));
        }
    }

    // Resolve A + AAAA per host.
    let mut out = Vec::with_capacity(ordered_hosts.len());
    for (priority, hostname) in ordered_hosts {
        let mut addrs: Vec<IpAddr> = Vec::new();
        if let Ok(v4) = resolver.lookup_a(&hostname).await {
            addrs.extend(v4.into_iter().map(IpAddr::V4));
        }
        if ipv6_enabled && let Ok(v6) = resolver.lookup_aaaa(&hostname).await {
            addrs.extend(v6.into_iter().map(IpAddr::V6));
        }
        if addrs.is_empty() {
            // No reachable addresses for this host — skip and let the
            // next priority cluster get a shot.
            continue;
        }
        out.push(MxHost {
            priority,
            hostname,
            addrs,
        });
    }
    Ok(out)
}

/// RFC 8305 Happy Eyeballs v2 — race v6 + v4 connects with a
/// `prefer_v6_delay` head-start for v6, return the first to succeed.
///
/// Implementation: spawn a v6 connect immediately if a v6 address is
/// present; after `prefer_v6_delay`, also spawn a v4 connect. Whichever
/// resolves `Ok` first wins; the other future is dropped. On v6-only
/// lists the v4 task is never spawned; on v4-only lists the v6 wait
/// reduces to zero. Empty list returns an error.
///
/// Returns the winning `TcpStream` and the `IpAddr` it connected to.
/// Callers needing per-attempt timeout enforcement wrap this in
/// `tokio::time::timeout(CONNECT_TIMEOUT, …)`.
pub async fn happy_eyeballs_connect(
    addrs: &[std::net::IpAddr],
    port: u16,
    prefer_v6_delay: Duration,
) -> anyhow::Result<(tokio::net::TcpStream, std::net::IpAddr)> {
    use std::net::SocketAddr;
    use tokio::net::TcpStream;

    if addrs.is_empty() {
        anyhow::bail!("happy_eyeballs_connect: no addresses to try");
    }

    // RFC 8305 §4: pick the first v6 and first v4 from the supplied
    // list. Multi-address per-family racing is left to the caller's
    // outer loop (the bridge iterates MX hosts; within one host one
    // address per family is the standard read of §4).
    let v6 = addrs.iter().copied().find(|a| a.is_ipv6());
    let v4 = addrs.iter().copied().find(|a| a.is_ipv4());

    match (v6, v4) {
        (Some(v6), Some(v4)) => {
            let v6_addr = SocketAddr::new(v6, port);
            let v4_addr = SocketAddr::new(v4, port);
            tokio::select! {
                biased;
                v6_res = TcpStream::connect(v6_addr) => match v6_res {
                    Ok(stream) => Ok((stream, v6)),
                    Err(_v6_err) => {
                        // v6 refused/unreachable — fall through to v4
                        // without waiting the preference delay.
                        let stream = TcpStream::connect(v4_addr).await?;
                        Ok((stream, v4))
                    }
                },
                v4_res = async {
                    tokio::time::sleep(prefer_v6_delay).await;
                    TcpStream::connect(v4_addr).await
                } => match v4_res {
                    Ok(stream) => Ok((stream, v4)),
                    Err(_v4_err) => {
                        // v4 lost the race but errored; fall through to v6.
                        let stream = TcpStream::connect(v6_addr).await?;
                        Ok((stream, v6))
                    }
                },
            }
        }
        (Some(v6), None) => {
            let stream = TcpStream::connect(SocketAddr::new(v6, port)).await?;
            Ok((stream, v6))
        }
        (None, Some(v4)) => {
            let stream = TcpStream::connect(SocketAddr::new(v4, port)).await?;
            Ok((stream, v4))
        }
        (None, None) => unreachable!("addrs non-empty but neither v4 nor v6"),
    }
}

// Suppress unused warnings for constants exposed for documentation /
// caller-side use even when not referenced by name from this module.
#[doc(hidden)]
const _USED: &[Duration] = &[
    CONNECT_TIMEOUT,
    PER_COMMAND_TIMEOUT,
    TOTAL_TIMEOUT,
    HAPPY_EYEBALLS_PREFER_V6_DELAY,
];

// ── DNSSEC-validating MX RRset resolver (nest-side, native-only) ──────

/// One recipient domain's SMTP targets, plus whether the **MX RRset they
/// came from was DNSSEC-validated**.
///
/// # Why the `secure` bit exists
///
/// Outbound DANE (RFC 7672) derives the TLSA base domain from the MX host.
/// RFC 7672 §2.2 is unambiguous that both legs must be secure: an SMTP
/// client whose MX RRset is not DNSSEC-validated MUST NOT treat the
/// destination as DANE-capable. Validating only the TLSA lookup is not
/// enough — an attacker who can spoof DNS (exactly the attacker DANE
/// exists to stop) forges `MX victim.test → mx.attacker.test`, publishes a
/// genuine DNSSEC-signed TLSA for **their own** name, and the pin then
/// succeeds *honestly* against the wrong host. The DNSSEC validation that
/// exists is satisfied; it is validating the name the attacker chose.
///
/// So the bit travels with the hosts, and the caller gates DANE on it.
/// `false` means DANE is not applicable — a provably Insecure RRset, or any
/// answer on a resolver path that does not carry DNSSEC
/// ([`crate::outbound::dnssec_path`]) — and narrows the posture to
/// MTA-STS / opportunistic TLS, exactly as an Insecure *TLSA* answer does.
/// A Bogus or Indeterminate answer on a DNSSEC-capable path is NOT a
/// `false`: it is a lookup failure that delays delivery (RFC 7672 §2.1.1,
/// §2.1.2; [`lookup_mx_on_path`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MxAnswer {
    /// `(priority, hostname)` pairs as published, unsorted.
    pub hosts: Vec<(u16, String)>,
    /// The MX RRset carried a DNSSEC `Secure` proof — or the domain
    /// publishes no MX RRset at all, so the SMTP target is the recipient
    /// domain itself (implicit MX, RFC 5321 §5.1) and no attacker-chosen
    /// name entered the decision. See [`MxAnswer`]'s own docs.
    pub secure: bool,
}

/// MX resolution behind a `dyn`-compatible boundary so nest can hold an
/// `Arc<dyn MxRrsetResolver>` and tests can substitute a mock. Mirrors
/// [`crate::outbound::dane::TlsaResolver`], and exists for the same stated
/// reason: the Go MTA bridge cannot do DNSSEC in the Go stdlib, so the
/// DNS work that needs a Rust capability happens nest-side
/// (`docs/goal/behavior/smtp-server.md` § Architectural rules).
#[async_trait]
pub trait MxRrsetResolver: Send + Sync {
    async fn lookup(&self, domain: &str) -> anyhow::Result<MxAnswer>;
}

/// Resolver that always answers "no hosts, not secure". Used by test
/// fixtures and any caller that doesn't model outbound delivery, so the
/// [`MxRrsetResolver`] boundary is always satisfiable (mirrors
/// [`crate::outbound::dane::NullTlsaResolver`]).
pub struct NullMxRrsetResolver;

#[async_trait]
impl MxRrsetResolver for NullMxRrsetResolver {
    async fn lookup(&self, _domain: &str) -> anyhow::Result<MxAnswer> {
        Ok(MxAnswer::default())
    }
}

/// Resolve `domain`'s MX RRset on the resolver path's posture
/// (`smtp-server.md` § MX resolution item 1, the Bogus / Indeterminate rule
/// and the resolver-path paragraph under it).
///
/// **Capable path** — validation on, and NEVER a re-read without it:
/// - every MX record proves `Secure` → hosts + `secure: true`;
/// - the RRset proves `Insecure` (an unsigned zone or an opt-out
///   delegation — a provable fact, not a failure) → hosts + `secure: false`,
///   so the caller skips DANE and falls back to MTA-STS / opportunistic TLS;
/// - any record whose proof is neither (Bogus, Indeterminate), a validation
///   error, SERVFAIL, a timeout → `Err`. RFC 7672 §2.1.1 equates a Bogus or
///   Indeterminate answer with lookup failure and §2.1.2 says an MX lookup
///   failure MUST delay delivery: re-reading the RRset without validation
///   would let a DNS spoofer who corrupts a DANE-protected recipient's signed
///   MX answer steer the mail, over opportunistic TLS, to a host they named;
/// - **no MX RRset** (NXDOMAIN / NoData) → implicit MX per RFC 5321 §5.1:
///   `[(0, domain)]` with `secure: true` (the target is the envelope's own
///   recipient domain, which no DNS answer chose — see [`MxAnswer`]).
///
/// **Incapable path** — the nest is a pre-DANE opportunistic-TLS MTA: the
/// lookup runs without validation and every answer, the implicit MX
/// included, travels at `secure: false`.
///
/// On either path a failed lookup is not a "no MX RR" answer (§ MX
/// resolution item 1): it propagates and the caller tempfails into the retry
/// curve rather than delivering to the domain's own A record.
#[cfg(feature = "outbound-net")]
pub async fn lookup_mx_on_path(domain: &str, path: ResolverPath) -> anyhow::Result<MxAnswer> {
    let name = domain.trim_end_matches('.');
    match path {
        ResolverPath::Capable => capable_mx_answer(name, raw_mx_lookup(name, true).await),
        ResolverPath::Incapable => incapable_mx_answer(name, raw_mx_lookup(name, false).await),
    }
}

#[cfg(feature = "outbound-net")]
/// One MX record's DNSSEC proof, as our policy reads hickory's `Proof`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecordProof {
    Secure,
    Insecure,
    /// Bogus, Indeterminate — or a record read without validation at all.
    Unproven,
}

#[cfg(feature = "outbound-net")]
/// One MX lookup's outcome before the [`MxAnswer`] policy is applied.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RawMx {
    /// NXDOMAIN / NoData, or an answer carrying no MX RDATA: the domain
    /// publishes no MX RRset.
    NoRecords,
    /// `(priority, exchange, proof)` per MX record.
    Records(Vec<(u16, String, RecordProof)>),
}

/// One MX lookup through hickory, DNSSEC validation on or off.
#[cfg(feature = "outbound-net")]
async fn raw_mx_lookup(name: &str, validate: bool) -> anyhow::Result<RawMx> {
    use hickory_resolver::TokioResolver;
    use hickory_resolver::proto::dnssec::Proof;
    use hickory_resolver::proto::rr::RData;

    // A validating read goes through the one TCP-only validating resolver
    // (`dnssec_path::validating_resolver`, smtp-server.md § MX resolution item
    // 1, the TCP paragraph); the incapable path's non-validating read keeps
    // the default transports (UDP, then TCP on a well-formed truncation), so a
    // resolver with no TCP at all still delivers mail, pre-DANE.
    let resolver = if validate {
        crate::outbound::dnssec_path::validating_resolver()?
    } else {
        TokioResolver::builder_tokio()
            .map_err(|e| anyhow::anyhow!("build resolver: {e}"))?
            .build()
            .map_err(|e| anyhow::anyhow!("build resolver: {e}"))?
    };

    let rrset = match resolver.mx_lookup(name).await {
        Ok(rrset) => rrset,
        // NXDOMAIN / NoData → the domain publishes no MX RRset, which is an
        // *answer*, not a failure: implicit MX.
        Err(e) if e.is_no_records_found() => return Ok(RawMx::NoRecords),
        Err(e) => return Err(anyhow::anyhow!("MX lookup failed: {e}")),
    };
    // hickory populates each record's per-record DNSSEC `proof` when
    // `opts.validate = true`; a record read without validation is unproven.
    let records: Vec<(u16, String, RecordProof)> = rrset
        .answers()
        .iter()
        .filter_map(|record| match &record.data {
            RData::MX(mx) => Some((
                mx.preference,
                mx.exchange.to_utf8().trim_end_matches('.').to_string(),
                match (validate, record.proof) {
                    (true, Proof::Secure) => RecordProof::Secure,
                    (true, Proof::Insecure) => RecordProof::Insecure,
                    _ => RecordProof::Unproven,
                },
            )),
            _ => None,
        })
        .collect();
    if records.is_empty() {
        // An MX lookup that succeeded but carried no MX RDATA is the same
        // "publishes no MX" answer.
        return Ok(RawMx::NoRecords);
    }
    Ok(RawMx::Records(records))
}

/// The capable-path [`MxAnswer`] policy over the validating lookup's outcome.
#[cfg(feature = "outbound-net")]
fn capable_mx_answer(name: &str, raw: anyhow::Result<RawMx>) -> anyhow::Result<MxAnswer> {
    let raw = raw.map_err(|e| {
        e.context(format!(
            "validating MX lookup for {name} failed; delivery is delayed, never retried \
             without validation (RFC 7672 §2.1.2)"
        ))
    })?;
    match raw {
        // Implicit MX (RFC 5321 §5.1): the target is the envelope's own
        // recipient domain, which no DNS answer chose — see `MxAnswer`.
        RawMx::NoRecords => Ok(MxAnswer {
            hosts: vec![(0, name.to_string())],
            secure: true,
        }),
        RawMx::Records(records) => {
            if records.iter().any(|(_, _, p)| *p == RecordProof::Unproven) {
                anyhow::bail!(
                    "MX RRset for {name} failed DNSSEC validation (Bogus / Indeterminate); \
                     delivery is delayed (RFC 7672 §2.1.1, §2.1.2)"
                );
            }
            let secure = records.iter().all(|(_, _, p)| *p == RecordProof::Secure);
            if !secure {
                tracing::info!(
                    domain = %name,
                    "MX RRset is provably Insecure; delivering without DANE (RFC 7672 §2.2)"
                );
            }
            Ok(MxAnswer {
                hosts: records.into_iter().map(|(p, h, _)| (p, h)).collect(),
                secure,
            })
        }
    }
}

/// The incapable-path [`MxAnswer`] policy: every answer at `secure: false`.
#[cfg(feature = "outbound-net")]
fn incapable_mx_answer(name: &str, raw: anyhow::Result<RawMx>) -> anyhow::Result<MxAnswer> {
    let hosts = match raw? {
        RawMx::NoRecords => vec![(0, name.to_string())],
        RawMx::Records(records) => records.into_iter().map(|(p, h, _)| (p, h)).collect(),
    };
    Ok(MxAnswer {
        hosts,
        secure: false,
    })
}

#[cfg(all(test, feature = "outbound-net"))]
mod path_policy_tests {
    use super::*;

    fn recs(proofs: &[RecordProof]) -> anyhow::Result<RawMx> {
        Ok(RawMx::Records(
            proofs
                .iter()
                .enumerate()
                .map(|(i, p)| (10 + i as u16, format!("mx{i}.beta.test"), *p))
                .collect(),
        ))
    }

    #[test]
    fn capable_secure_rrset_is_secure() {
        let a = capable_mx_answer("beta.test", recs(&[RecordProof::Secure])).unwrap();
        assert_eq!(a.hosts, vec![(10, "mx0.beta.test".to_string())]);
        assert!(a.secure);
    }

    #[test]
    fn capable_insecure_rrset_delivers_without_dane() {
        let a = capable_mx_answer(
            "beta.test",
            recs(&[RecordProof::Secure, RecordProof::Insecure]),
        )
        .unwrap();
        assert_eq!(a.hosts.len(), 2);
        assert!(!a.secure);
    }

    #[test]
    fn capable_unproven_record_is_a_lookup_failure() {
        // A Bogus / Indeterminate record anywhere in the RRset delays
        // delivery — never a delivery at `secure: false`.
        let r = capable_mx_answer(
            "beta.test",
            recs(&[RecordProof::Secure, RecordProof::Unproven]),
        );
        assert!(r.is_err());
    }

    #[test]
    fn capable_validation_error_propagates_and_is_never_reread() {
        // There is no plain re-read to fall back on: the policy only ever sees
        // the validating lookup's own outcome.
        let r = capable_mx_answer(
            "beta.test",
            Err(anyhow::anyhow!(
                "MX lookup failed: DNSSEC validation failed"
            )),
        );
        let msg = format!("{:#}", r.unwrap_err());
        assert!(msg.contains("delayed"), "{msg}");
        assert!(msg.contains("DNSSEC validation failed"), "{msg}");
    }

    #[test]
    fn capable_no_records_is_the_secure_implicit_mx() {
        let a = capable_mx_answer("beta.test", Ok(RawMx::NoRecords)).unwrap();
        assert_eq!(a.hosts, vec![(0, "beta.test".to_string())]);
        assert!(a.secure);
    }

    #[test]
    fn incapable_answers_are_never_secure_implicit_mx_included() {
        let a = incapable_mx_answer("beta.test", recs(&[RecordProof::Unproven])).unwrap();
        assert_eq!(a.hosts, vec![(10, "mx0.beta.test".to_string())]);
        assert!(!a.secure);
        let a = incapable_mx_answer("beta.test", Ok(RawMx::NoRecords)).unwrap();
        assert_eq!(a.hosts, vec![(0, "beta.test".to_string())]);
        assert!(!a.secure);
    }

    #[test]
    fn incapable_lookup_failure_is_an_error_not_an_implicit_mx() {
        assert!(incapable_mx_answer("beta.test", Err(anyhow::anyhow!("timed out"))).is_err());
    }
}

/// Live [`MxRrsetResolver`] over [`lookup_mx_on_path`], consulting the
/// process's [`DnssecPath`] posture on every lookup. The production outbound
/// path holds an `Arc<dyn MxRrsetResolver>`; this is the network-touching
/// impl (mirrors [`crate::outbound::dane::LiveTlsaResolver`], which shares
/// the same `DnssecPath`).
#[cfg(feature = "outbound-net")]
pub struct LiveMxRrsetResolver {
    path: Arc<DnssecPath>,
}

#[cfg(feature = "outbound-net")]
impl LiveMxRrsetResolver {
    pub fn new(path: Arc<DnssecPath>) -> Self {
        Self { path }
    }
}

#[cfg(feature = "outbound-net")]
#[async_trait]
impl MxRrsetResolver for LiveMxRrsetResolver {
    async fn lookup(&self, domain: &str) -> anyhow::Result<MxAnswer> {
        lookup_mx_on_path(domain, self.path.posture().await).await
    }
}
