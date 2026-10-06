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
/// `false` is the fail-safe value: an answer of unknown provenance is
/// treated as not-DANE-capable, falling back to the MTA-STS /
/// opportunistic posture exactly as an Insecure *TLSA* answer already
/// does. Falling back is the correct direction — a non-secure MX answer
/// must **skip** DANE, never fail delivery.
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

/// Resolve `domain`'s MX RRset with mandatory DNSSEC validation, reporting
/// both the hosts and whether the RRset proved `Secure`.
///
/// Outcomes:
/// - MX RRset present, every returned record proves `Secure` → hosts +
///   `secure: true`.
/// - MX RRset present but the proof is Insecure / Bogus / Indeterminate →
///   the hosts are still returned (delivery must proceed) with
///   `secure: false`, so the caller skips DANE and falls back to the
///   MTA-STS / opportunistic posture.
/// - **No MX RRset** (NXDOMAIN / NoData) → implicit MX per RFC 5321 §5.1:
///   `[(0, domain)]` with `secure: true`. The SMTP target is the envelope's
///   own recipient domain, not a name any DNS answer chose, so the
///   RFC 7672 §2.2 concern — a forged MX indirection — cannot apply. An
///   attacker who strips a real MX RRset gains nothing: reaching DANE at
///   the apex still requires a `Secure` TLSA at `_25._tcp.<domain>`, which
///   they cannot forge for a signed zone and which does not exist for an
///   unsigned one.
/// - Resolver / network error → propagated. **A failed lookup is not a
///   "no MX RR" answer** (§ MX resolution item 1): the caller tempfails
///   into the retry curve rather than delivering to the domain's own A
///   record.
///
/// Native-only (`outbound-net` feature): `hickory-resolver` does not
/// cross-compile to wasm32 and must not enter the client FFI surface.
#[cfg(feature = "outbound-net")]
pub async fn lookup_mx_secure(domain: &str) -> anyhow::Result<MxAnswer> {
    use hickory_resolver::TokioResolver;
    use hickory_resolver::config::ResolverOpts;
    use hickory_resolver::proto::dnssec::Proof;
    use hickory_resolver::proto::rr::RData;

    let mut opts = ResolverOpts::default();
    opts.validate = true; // Require DNSSEC validation for the chain.
    let resolver = TokioResolver::builder_tokio()
        .map_err(|e| anyhow::anyhow!("build resolver: {e}"))?
        .with_options(opts)
        .build()
        .map_err(|e| anyhow::anyhow!("build resolver: {e}"))?;

    let name = domain.trim_end_matches('.');
    let rrset = match resolver.mx_lookup(name).await {
        Ok(rrset) => rrset,
        Err(e) => {
            // NXDOMAIN / NoData → the domain publishes no MX RRset, which
            // is an *answer*, not a failure: implicit MX. Every other
            // error propagates (see the doc comment).
            if e.is_no_records_found() {
                return Ok(MxAnswer {
                    hosts: vec![(0, name.to_string())],
                    secure: true,
                });
            }
            return Err(anyhow::anyhow!("MX lookup failed: {e}"));
        }
    };

    let mut hosts = Vec::new();
    let mut all_secure = true;
    let mut saw_record = false;
    // Mirrors `dane::lookup_tlsa`'s walk: hickory populates each record's
    // per-record DNSSEC `proof` because `opts.validate = true`.
    for record in rrset.answers() {
        if let RData::MX(mx) = &record.data {
            saw_record = true;
            if record.proof != Proof::Secure {
                all_secure = false;
            }
            hosts.push((
                mx.preference,
                mx.exchange.to_utf8().trim_end_matches('.').to_string(),
            ));
        }
    }

    if !saw_record {
        // An MX lookup that succeeded but carried no MX RDATA is the same
        // "publishes no MX" answer the NoData branch above handles.
        return Ok(MxAnswer {
            hosts: vec![(0, name.to_string())],
            secure: true,
        });
    }

    if !all_secure {
        tracing::warn!(
            domain = %name,
            "MX RRset is not DNSSEC-secure; delivering without DANE (RFC 7672 §2.2)"
        );
    }

    Ok(MxAnswer {
        hosts,
        secure: all_secure,
    })
}

/// Live [`MxRrsetResolver`] over the DNSSEC-validating [`lookup_mx_secure`].
/// The production outbound path holds an `Arc<dyn MxRrsetResolver>`; this is
/// the network-touching impl (mirrors
/// [`crate::outbound::dane::LiveTlsaResolver`]).
#[cfg(feature = "outbound-net")]
pub struct LiveMxRrsetResolver;

#[cfg(feature = "outbound-net")]
#[async_trait]
impl MxRrsetResolver for LiveMxRrsetResolver {
    async fn lookup(&self, domain: &str) -> anyhow::Result<MxAnswer> {
        lookup_mx_secure(domain).await
    }
}
