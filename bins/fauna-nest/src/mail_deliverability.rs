//! Deliverability-diagnostic orchestration + the outbound-STARTTLS prober seam.
//!
//! The Admin-class `fauna.bridges.run_deliverability_diagnostics` +
//! `fauna.bridges.blocklist_self_check_run` handlers + the 24h blocklist-self-
//! check timer call into here. The orchestrator runs the check set of
//! `docs/goal/behavior/mail-deliverability.md` § Symptom diagnostics over two
//! injected I/O seams — the nest `RecordResolver` (DNS) and a [`StarttlsProber`]
//! (the gmail `:25` posture probe) — plus precomputed inputs (the MTA-STS
//! policy-file fetch, which nest does via its existing `MtaStsFetcher`), and
//! interprets each via the pure `fauna_mail::deliverability` verdicts. Seams +
//! precomputed inputs keep it fully unit-testable with a scripted resolver +
//! fake prober (the tests at the bottom), with no real network.
//!
//! Caller class is **Admin**, not MTA: the diagnostic is admin-pane-only
//! (`mail-deliverability.md` § Wire shapes names the admin-client caller;
//! § Architectural rules: "Symptom diagnostic is admin-pane-only").

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use fauna_mail::deliverability::{
    self, DmarcMode, GMAIL_PROBE_MX_HOST, GMAIL_PROBE_MX_PORT, dnsbl_listed, dnsbl_query_name,
    reverse_dns_ptr_name,
};
use fauna_protocol::bridge_routing::{BlocklistServerResult, DiagnosticCheckResult};

use crate::dns_verifier::{LookupOutcome, RecordResolver};

// ── STARTTLS prober seam ───────────────────────────────────────────

/// Outcome of the outbound-TLS posture probe to a known peer.
pub enum StarttlsProbeOutcome {
    /// Connected, STARTTLS succeeded, and the cert chain verified.
    Ok,
    /// Connection refused / STARTTLS rejected / cert path invalid / timeout —
    /// the string is the admin-facing failure reason.
    Failed(String),
}

/// The `dyn`-compatible outbound-STARTTLS seam (mirrors `MtaStsFetcher` /
/// `TlsaResolver`): production installs [`LiveStarttlsProber`]; `for_test` (and
/// any build without outbound `:25`) installs [`NullStarttlsProber`].
#[async_trait]
pub trait StarttlsProber: Send + Sync {
    /// Connect to `host:port`, EHLO, STARTTLS, and verify the TLS cert chain.
    async fn probe(&self, host: &str, port: u16) -> StarttlsProbeOutcome;
}

/// Prober that reports the probe as unavailable (a Warn-class failure), never
/// touching the network — installed in `AppState::for_test`.
pub struct NullStarttlsProber;

#[async_trait]
impl StarttlsProber for NullStarttlsProber {
    async fn probe(&self, _host: &str, _port: u16) -> StarttlsProbeOutcome {
        StarttlsProbeOutcome::Failed("outbound STARTTLS probe not available in this build".into())
    }
}

/// Live prober: TCP-connects, reads the SMTP banner, EHLOs, issues STARTTLS, and
/// completes a cert-verifying TLS handshake (webpki roots). The whole exchange
/// is bounded by [`PROBE_TIMEOUT`]. A failure at any step → `Failed(reason)`.
pub struct LiveStarttlsProber;

/// Wall-clock bound on the whole probe (connect + SMTP handshake + TLS). The
/// common CI/cloud failure — outbound `:25` blocked — usually surfaces as a
/// connect refusal (fast) but can hang silently; this bounds it.
const PROBE_TIMEOUT: Duration = Duration::from_secs(12);

#[async_trait]
impl StarttlsProber for LiveStarttlsProber {
    async fn probe(&self, host: &str, port: u16) -> StarttlsProbeOutcome {
        match tokio::time::timeout(PROBE_TIMEOUT, probe_starttls(host, port)).await {
            Ok(Ok(())) => StarttlsProbeOutcome::Ok,
            Ok(Err(e)) => StarttlsProbeOutcome::Failed(e.to_string()),
            Err(_) => StarttlsProbeOutcome::Failed(format!(
                "timed out after {}s connecting to {host}:{port}",
                PROBE_TIMEOUT.as_secs()
            )),
        }
    }
}

async fn probe_starttls(host: &str, port: u16) -> anyhow::Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let stream = tokio::net::TcpStream::connect((host, port)).await?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();

    // 220 service banner.
    reader.read_line(&mut line).await?;
    if !line.starts_with("220") {
        anyhow::bail!("unexpected SMTP banner: {}", line.trim());
    }

    // EHLO; scan the (possibly multi-line) reply for the STARTTLS extension.
    reader
        .get_mut()
        .write_all(b"EHLO fauna-deliverability-probe\r\n")
        .await?;
    let mut advertises_starttls = false;
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            anyhow::bail!("connection closed during EHLO");
        }
        if line.to_ascii_uppercase().contains("STARTTLS") {
            advertises_starttls = true;
        }
        // RFC 5321 §4.2.1: `250-` continues, `250 ` (space) is the final line.
        if line.starts_with("250 ") {
            break;
        }
        if !line.starts_with("250") {
            anyhow::bail!("EHLO rejected: {}", line.trim());
        }
    }
    if !advertises_starttls {
        anyhow::bail!("peer did not advertise STARTTLS");
    }

    // STARTTLS → expect 220, then the TLS handshake (which verifies the chain).
    reader.get_mut().write_all(b"STARTTLS\r\n").await?;
    line.clear();
    reader.read_line(&mut line).await?;
    if !line.starts_with("220") {
        anyhow::bail!("STARTTLS rejected: {}", line.trim());
    }

    let tcp = reader.into_inner();
    // Straight from the shared rustls preamble, not through the WS transport
    // crate: this probe is a plain STARTTLS dial and has no business depending
    // on `fauna-ws-substrate` for a root store (2026-08-30 — the store moved to
    // `fauna-tls-bootstrap`, which is the crate that exists for exactly this).
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(fauna_tls_bootstrap::webpki_root_store())
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let server_name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|e| anyhow::anyhow!("invalid server name {host}: {e}"))?;
    // A bad/expired/untrusted chain fails the handshake here.
    connector.connect(server_name, tcp).await?;
    Ok(())
}

// ── Force-refresh rate limit ───────────────────────────────────────

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Process-global last-force-refresh time (epoch-seconds) per DNSBL server. The
/// deployment is one nest process, so a static suffices for the 1/min/DNSBL
/// force-refresh cap (`mail-deliverability.md` § Force-refresh + § Don't do
/// these — defends against a button-mashing admin getting the deployment IP
/// listed for "abusive query patterns"). The 24h-timer sweep calls
/// [`run_blocklist_self_check`] directly and is not gated by this.
static FORCE_REFRESH_LAST: OnceLock<Mutex<HashMap<String, i64>>> = OnceLock::new();

/// `true` (and records `now_secs`) iff `server` hasn't been force-refreshed in
/// the last 60s; `false` (rate-limited) otherwise.
pub fn force_refresh_allowed(server: &str, now_secs: i64) -> bool {
    let map = FORCE_REFRESH_LAST.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().unwrap();
    match guard.get(server) {
        Some(&last) if now_secs.saturating_sub(last) < 60 => false,
        _ => {
            guard.insert(server.to_string(), now_secs);
            true
        }
    }
}

// ── Diagnostic orchestration ───────────────────────────────────────

/// One DKIM selector the deployment published, with the expected public TXT
/// value (`public_dns_value` from `list_dkim_selectors`). One entry per active
/// selector = one entry per algorithm (`mail-deliverability.md` "DKIM record per
/// algorithm").
pub struct DkimExpectation {
    pub selector: String,
    pub public_dns_value: String,
}

/// Everything the diagnostic orchestrator needs that nest sources from state.
pub struct DiagnosticInput {
    /// The deployment's primary mail domain (e.g. `example.com`).
    pub primary_domain: String,
    /// The HELO name the MTA sends (`mail.<primary_domain>`) — the PTR target.
    pub helo_name: String,
    /// The outbound IP (resolved A/AAAA of `helo_name`), or `None` if it
    /// couldn't be resolved (the reverse-DNS rows then fail with that reason).
    pub outbound_ip: Option<IpAddr>,
    /// Active DKIM selectors + their expected public TXT value.
    pub dkim_selectors: Vec<DkimExpectation>,
    /// Precomputed MTA-STS policy-file fetch (nest does the HTTPS GET via its
    /// existing `MtaStsFetcher`): `Ok(())` = fetched + valid; `Err(reason)` =
    /// not-fetchable / parse error. The *record-present* row is checked via DNS.
    pub mta_sts_policy_fetch: Result<(), String>,
}

/// Resolve the deployment's outbound IP from its advertised mail host
/// (`mail.<primary>` A/AAAA) — the IP whose PTR + DNSBL listing the diagnostic
/// checks. Single-homed VPS assumption (inbound MX IP == outbound IP); a split
/// outbound IP is a documented follow-on. `None` if neither A nor AAAA resolves.
pub async fn resolve_outbound_ip(resolver: &dyn RecordResolver, helo_name: &str) -> Option<IpAddr> {
    for rtype in ["A", "AAAA"] {
        if let LookupOutcome::Records(recs) = resolver.lookup(helo_name, rtype).await
            && let Some(ip) = recs.iter().find_map(|r| r.trim().parse::<IpAddr>().ok())
        {
            return Some(ip);
        }
    }
    None
}

fn pass(name: &str, detail: impl Into<String>) -> DiagnosticCheckResult {
    DiagnosticCheckResult {
        name: name.into(),
        status: "pass".into(),
        detail: detail.into(),
    }
}
fn warn(name: &str, detail: impl Into<String>) -> DiagnosticCheckResult {
    DiagnosticCheckResult {
        name: name.into(),
        status: "warn".into(),
        detail: detail.into(),
    }
}
fn fail(name: &str, detail: impl Into<String>) -> DiagnosticCheckResult {
    DiagnosticCheckResult {
        name: name.into(),
        status: "fail".into(),
        detail: detail.into(),
    }
}

/// Resolve `name`/`rtype`, returning the records or `None` on a transient
/// (retryable) lookup failure — the caller renders `None` as a Warn row.
async fn lookup_records(
    resolver: &dyn RecordResolver,
    name: &str,
    rtype: &str,
) -> Option<Vec<String>> {
    match resolver.lookup(name, rtype).await {
        LookupOutcome::Records(v) => Some(v),
        LookupOutcome::Empty => Some(Vec::new()),
        LookupOutcome::Transient => None,
    }
}

fn norm_host(h: &str) -> String {
    fauna_core::web::normalize_dns_name(h)
}

/// Run the deliverability diagnostic checklist. Pure over the two seams + the
/// precomputed MTA-STS fetch; no real network here.
pub async fn run_diagnostics(
    resolver: &dyn RecordResolver,
    prober: &dyn StarttlsProber,
    input: &DiagnosticInput,
) -> Vec<DiagnosticCheckResult> {
    let mut out = Vec::new();
    let domain = &input.primary_domain;

    // ── SPF ──
    match lookup_records(resolver, domain, "TXT").await {
        None => {
            out.push(warn("SPF record present", "DNS lookup failed (transient)"));
            out.push(warn("SPF record valid", "DNS lookup failed (transient)"));
        }
        Some(txt) => {
            let lint = deliverability::lint_spf(&txt);
            if !lint.found {
                out.push(fail("SPF record present", "no v=spf1 record at the apex"));
                out.push(fail("SPF record valid", "no SPF record to validate"));
            } else {
                if lint.includes_mx {
                    out.push(pass("SPF record present", "v=spf1 present and includes mx"));
                } else {
                    out.push(fail(
                        "SPF record present",
                        "SPF present but missing the `mx` mechanism this deployment relies on",
                    ));
                }
                if lint.too_many_lookups {
                    out.push(fail(
                        "SPF record valid",
                        format!(
                            "{} DNS-lookup mechanisms exceeds the RFC 7208 §4.6.4 limit of 10",
                            lint.lookup_count
                        ),
                    ));
                } else {
                    out.push(pass(
                        "SPF record valid",
                        format!("{} DNS-lookup mechanisms (≤ 10)", lint.lookup_count),
                    ));
                }
            }
        }
    }

    // ── DKIM (per selector / algorithm) ──
    if input.dkim_selectors.is_empty() {
        out.push(warn(
            "DKIM record present",
            "no active DKIM selector provisioned for this domain",
        ));
    }
    for sel in &input.dkim_selectors {
        let name = format!("{}._domainkey.{domain}", sel.selector);
        let label = format!("DKIM record present ({})", sel.selector);
        match lookup_records(resolver, &name, "TXT").await {
            None => out.push(warn(&label, "DNS lookup failed (transient)")),
            Some(txt) => {
                if !deliverability::dkim_record_present(&txt) {
                    out.push(fail(&label, format!("no DKIM TXT at {name}")));
                } else if deliverability::dkim_pubkey_matches(&txt, &sel.public_dns_value) {
                    out.push(pass(&label, "published key matches the deployment's key"));
                } else {
                    out.push(fail(
                        &label,
                        "published key does not match the deployment's key",
                    ));
                }
            }
        }
    }

    // ── DMARC ──
    match lookup_records(resolver, &format!("_dmarc.{domain}"), "TXT").await {
        None => {
            out.push(warn(
                "DMARC record present",
                "DNS lookup failed (transient)",
            ));
            out.push(warn(
                "DMARC policy enforcing",
                "DNS lookup failed (transient)",
            ));
        }
        Some(txt) => {
            if !deliverability::dmarc_record_present(&txt) {
                out.push(fail("DMARC record present", "no v=DMARC1 record at _dmarc"));
                out.push(fail("DMARC policy enforcing", "no DMARC record"));
            } else {
                out.push(pass("DMARC record present", "v=DMARC1 present"));
                match deliverability::dmarc_policy_mode(&txt) {
                    Some(DmarcMode::Reject) => out.push(pass("DMARC policy enforcing", "p=reject")),
                    Some(DmarcMode::Quarantine) => {
                        out.push(pass("DMARC policy enforcing", "p=quarantine"))
                    }
                    Some(DmarcMode::None) => out.push(warn(
                        "DMARC policy enforcing",
                        "p=none (monitor-only — not enforcing)",
                    )),
                    None => out.push(warn(
                        "DMARC policy enforcing",
                        "DMARC record present but no parseable p= policy",
                    )),
                }
            }
        }
    }

    // ── MTA-STS record present + policy file fetchable ──
    match lookup_records(resolver, &format!("_mta-sts.{domain}"), "TXT").await {
        None => out.push(warn(
            "MTA-STS record present",
            "DNS lookup failed (transient)",
        )),
        Some(txt) => {
            if deliverability::mta_sts_record_present(&txt) {
                out.push(pass("MTA-STS record present", "v=STSv1 with id="));
            } else {
                out.push(fail(
                    "MTA-STS record present",
                    "no v=STSv1 record at _mta-sts",
                ));
            }
        }
    }
    match &input.mta_sts_policy_fetch {
        Ok(()) => out.push(pass(
            "MTA-STS policy file fetchable",
            "https://mta-sts.<domain>/.well-known/mta-sts.txt fetched + valid",
        )),
        Err(reason) => out.push(fail("MTA-STS policy file fetchable", reason.clone())),
    }

    // ── TLSRPT ──
    match lookup_records(resolver, &format!("_smtp._tls.{domain}"), "TXT").await {
        None => out.push(warn(
            "TLSRPT record present",
            "DNS lookup failed (transient)",
        )),
        Some(txt) => {
            if deliverability::tlsrpt_record_present(&txt) {
                out.push(pass("TLSRPT record present", "v=TLSRPTv1 present"));
            } else {
                out.push(fail(
                    "TLSRPT record present",
                    "no v=TLSRPTv1 record at _smtp._tls",
                ));
            }
        }
    }

    // ── Reverse-DNS for the outbound IP + matches HELO ──
    match input.outbound_ip {
        None => {
            let detail = format!(
                "couldn't resolve the outbound IP from {} A/AAAA",
                input.helo_name
            );
            out.push(fail("Reverse-DNS for outbound IP", detail.clone()));
            out.push(fail("Reverse-DNS matches HELO", detail));
        }
        Some(ip) => {
            let ptr_name = reverse_dns_ptr_name(ip);
            match lookup_records(resolver, &ptr_name, "PTR").await {
                None => {
                    out.push(warn(
                        "Reverse-DNS for outbound IP",
                        "DNS lookup failed (transient)",
                    ));
                    out.push(warn(
                        "Reverse-DNS matches HELO",
                        "DNS lookup failed (transient)",
                    ));
                }
                Some(ptrs) if ptrs.is_empty() => {
                    out.push(fail(
                        "Reverse-DNS for outbound IP",
                        format!("no PTR record for {ip}"),
                    ));
                    out.push(fail(
                        "Reverse-DNS matches HELO",
                        "no PTR record to compare against the HELO name",
                    ));
                }
                Some(ptrs) => {
                    let observed = norm_host(&ptrs[0]);
                    out.push(pass(
                        "Reverse-DNS for outbound IP",
                        format!("{ip} → {observed}"),
                    ));
                    let helo = norm_host(&input.helo_name);
                    if ptrs.iter().any(|p| norm_host(p) == helo) {
                        out.push(pass("Reverse-DNS matches HELO", format!("PTR == {helo}")));
                    } else {
                        out.push(fail(
                            "Reverse-DNS matches HELO",
                            format!(
                                "PTR {observed} != HELO {helo} (fixable at the VPS provider's networking page)"
                            ),
                        ));
                    }
                }
            }
        }
    }

    // ── Outbound TLS posture probe to gmail ──
    match prober.probe(GMAIL_PROBE_MX_HOST, GMAIL_PROBE_MX_PORT).await {
        StarttlsProbeOutcome::Ok => out.push(pass(
            "Outbound TLS to gmail.com",
            format!(
                "STARTTLS to {GMAIL_PROBE_MX_HOST}:{GMAIL_PROBE_MX_PORT} succeeded, chain valid"
            ),
        )),
        StarttlsProbeOutcome::Failed(reason) => {
            // Outbound :25 unreachable is a deployment-network fact (the probe
            // can't fix it) — a Warn, not a hard Fail, distinguishes "we can't
            // see" from "their TLS is broken".
            out.push(warn("Outbound TLS to gmail.com", reason))
        }
    }

    out
}

/// Run the DNSBL self-check for `ip` against `servers` (or just `only_server`,
/// for the admin force-refresh path). `ip = None` → every server is an error
/// row (we couldn't resolve our own outbound IP to check).
pub async fn run_blocklist_self_check(
    resolver: &dyn RecordResolver,
    ip: Option<IpAddr>,
    servers: &[String],
    only_server: Option<&str>,
) -> Vec<BlocklistServerResult> {
    let mut out = Vec::new();
    for server in servers {
        if let Some(only) = only_server
            && only != server
        {
            continue;
        }
        let Some(ip) = ip else {
            out.push(BlocklistServerResult {
                server: server.clone(),
                listed: false,
                reason: String::new(),
                error: "outbound IP unresolved".into(),
            });
            continue;
        };
        let query = dnsbl_query_name(ip, server);
        match resolver.lookup(&query, "A").await {
            LookupOutcome::Transient => out.push(BlocklistServerResult {
                server: server.clone(),
                listed: false,
                reason: String::new(),
                error: "DNSBL query failed (timeout/SERVFAIL)".into(),
            }),
            LookupOutcome::Empty => out.push(BlocklistServerResult {
                server: server.clone(),
                listed: false,
                reason: String::new(),
                error: String::new(),
            }),
            LookupOutcome::Records(a) => {
                let listed = dnsbl_listed(&a);
                let reason = if listed {
                    match resolver.lookup(&query, "TXT").await {
                        LookupOutcome::Records(txt) => txt.join(" "),
                        _ => String::new(),
                    }
                } else {
                    String::new()
                };
                out.push(BlocklistServerResult {
                    server: server.clone(),
                    listed,
                    reason,
                    error: String::new(),
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Scripted resolver: returns a programmed `LookupOutcome` per `(name, TYPE)`
    /// (name lowercased, trailing dot stripped); unknown lookups → `Empty`.
    struct ScriptedResolver {
        answers: Mutex<HashMap<(String, String), LookupOutcome>>,
    }
    impl ScriptedResolver {
        fn new() -> Self {
            Self {
                answers: Mutex::new(HashMap::new()),
            }
        }
        fn set(&self, name: &str, rtype: &str, outcome: LookupOutcome) {
            self.answers
                .lock()
                .unwrap()
                .insert((norm_host(name), rtype.to_ascii_uppercase()), outcome);
        }
    }
    #[async_trait]
    impl RecordResolver for ScriptedResolver {
        async fn lookup(&self, name: &str, rtype: &str) -> LookupOutcome {
            self.answers
                .lock()
                .unwrap()
                .get(&(norm_host(name), rtype.to_ascii_uppercase()))
                .cloned()
                .unwrap_or(LookupOutcome::Empty)
        }
    }

    struct FakeProber(bool);
    #[async_trait]
    impl StarttlsProber for FakeProber {
        async fn probe(&self, _host: &str, _port: u16) -> StarttlsProbeOutcome {
            if self.0 {
                StarttlsProbeOutcome::Ok
            } else {
                StarttlsProbeOutcome::Failed("connection refused".into())
            }
        }
    }

    fn rec(values: &[&str]) -> LookupOutcome {
        LookupOutcome::Records(values.iter().map(|s| s.to_string()).collect())
    }

    fn status_of<'a>(checks: &'a [DiagnosticCheckResult], name: &str) -> &'a str {
        &checks
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("missing check {name}"))
            .status
    }

    #[tokio::test]
    async fn all_green_deployment() {
        let r = ScriptedResolver::new();
        let p = "CcxUQyfB/vzMnyjSzF/sK/WDzuNE3Be/sAqpqOWXrss=";
        r.set("example.com", "TXT", rec(&["v=spf1 mx ~all"]));
        r.set(
            "sel1._domainkey.example.com",
            "TXT",
            rec(&[&format!("v=DKIM1; k=ed25519; p={p}")]),
        );
        r.set("_dmarc.example.com", "TXT", rec(&["v=DMARC1; p=reject"]));
        r.set(
            "_mta-sts.example.com",
            "TXT",
            rec(&["v=STSv1; id=20260612"]),
        );
        r.set(
            "_smtp._tls.example.com",
            "TXT",
            rec(&["v=TLSRPTv1; rua=mailto:t@example.com"]),
        );
        // mail.example.com → 203.0.113.7 → PTR back to mail.example.com (FCrDNS).
        r.set(
            "7.113.0.203.in-addr.arpa",
            "PTR",
            rec(&["mail.example.com."]),
        );

        let input = DiagnosticInput {
            primary_domain: "example.com".into(),
            helo_name: "mail.example.com".into(),
            outbound_ip: Some("203.0.113.7".parse().unwrap()),
            dkim_selectors: vec![DkimExpectation {
                selector: "sel1".into(),
                public_dns_value: format!("v=DKIM1; k=ed25519; p={p}"),
            }],
            mta_sts_policy_fetch: Ok(()),
        };
        let checks = run_diagnostics(&r, &FakeProber(true), &input).await;
        assert_eq!(status_of(&checks, "SPF record present"), "pass");
        assert_eq!(status_of(&checks, "SPF record valid"), "pass");
        assert_eq!(status_of(&checks, "DKIM record present (sel1)"), "pass");
        assert_eq!(status_of(&checks, "DMARC record present"), "pass");
        assert_eq!(status_of(&checks, "DMARC policy enforcing"), "pass");
        assert_eq!(status_of(&checks, "MTA-STS record present"), "pass");
        assert_eq!(status_of(&checks, "MTA-STS policy file fetchable"), "pass");
        assert_eq!(status_of(&checks, "TLSRPT record present"), "pass");
        assert_eq!(status_of(&checks, "Reverse-DNS for outbound IP"), "pass");
        assert_eq!(status_of(&checks, "Reverse-DNS matches HELO"), "pass");
        assert_eq!(status_of(&checks, "Outbound TLS to gmail.com"), "pass");
        assert!(checks.iter().all(|c| c.status == "pass"));
    }

    #[tokio::test]
    async fn problems_surface_as_fail_warn() {
        let r = ScriptedResolver::new();
        // SPF without mx; DMARC p=none; no DKIM/MTA-STS/TLSRPT; PTR mismatch.
        r.set(
            "example.com",
            "TXT",
            rec(&["v=spf1 include:_spf.other.com ~all"]),
        );
        r.set("_dmarc.example.com", "TXT", rec(&["v=DMARC1; p=none"]));
        r.set(
            "7.113.0.203.in-addr.arpa",
            "PTR",
            rec(&["vps-1.provider.test."]),
        );

        let input = DiagnosticInput {
            primary_domain: "example.com".into(),
            helo_name: "mail.example.com".into(),
            outbound_ip: Some("203.0.113.7".parse().unwrap()),
            dkim_selectors: vec![DkimExpectation {
                selector: "sel1".into(),
                public_dns_value: "v=DKIM1; k=ed25519; p=ZZZ=".into(),
            }],
            mta_sts_policy_fetch: Err("404 fetching the policy file".into()),
        };
        let checks = run_diagnostics(&r, &FakeProber(false), &input).await;
        assert_eq!(status_of(&checks, "SPF record present"), "fail"); // no mx
        assert_eq!(status_of(&checks, "SPF record valid"), "pass"); // 1 lookup ≤ 10
        assert_eq!(status_of(&checks, "DKIM record present (sel1)"), "fail"); // no record
        assert_eq!(status_of(&checks, "DMARC policy enforcing"), "warn"); // p=none
        assert_eq!(status_of(&checks, "MTA-STS record present"), "fail");
        assert_eq!(status_of(&checks, "MTA-STS policy file fetchable"), "fail");
        assert_eq!(status_of(&checks, "TLSRPT record present"), "fail");
        assert_eq!(status_of(&checks, "Reverse-DNS matches HELO"), "fail"); // PTR mismatch
        assert_eq!(status_of(&checks, "Outbound TLS to gmail.com"), "warn"); // probe failed
    }

    #[tokio::test]
    async fn unresolved_outbound_ip_fails_reverse_dns() {
        let r = ScriptedResolver::new();
        let input = DiagnosticInput {
            primary_domain: "example.com".into(),
            helo_name: "mail.example.com".into(),
            outbound_ip: None,
            dkim_selectors: vec![],
            mta_sts_policy_fetch: Ok(()),
        };
        let checks = run_diagnostics(&r, &FakeProber(true), &input).await;
        assert_eq!(status_of(&checks, "Reverse-DNS for outbound IP"), "fail");
        assert_eq!(status_of(&checks, "Reverse-DNS matches HELO"), "fail");
        // No DKIM selectors → a single warn row, not a crash.
        assert_eq!(status_of(&checks, "DKIM record present"), "warn");
    }

    #[tokio::test]
    async fn blocklist_self_check_listed_and_clean() {
        let r = ScriptedResolver::new();
        let ip: IpAddr = "203.0.113.7".parse().unwrap();
        // zen: not listed (NXDOMAIN → Empty). barracuda: listed 127.0.0.2 + reason.
        r.set(
            "7.113.0.203.b.barracudacentral.org",
            "A",
            rec(&["127.0.0.2"]),
        );
        r.set(
            "7.113.0.203.b.barracudacentral.org",
            "TXT",
            rec(&["Listed for spam pattern X"]),
        );
        let servers: Vec<String> = vec!["zen.spamhaus.org".into(), "b.barracudacentral.org".into()];
        let results = run_blocklist_self_check(&r, Some(ip), &servers, None).await;
        assert_eq!(results.len(), 2);
        let zen = results
            .iter()
            .find(|x| x.server == "zen.spamhaus.org")
            .unwrap();
        assert!(!zen.listed && zen.error.is_empty());
        let barr = results
            .iter()
            .find(|x| x.server == "b.barracudacentral.org")
            .unwrap();
        assert!(barr.listed);
        assert_eq!(barr.reason, "Listed for spam pattern X");

        // Force-refresh a single server.
        let one = run_blocklist_self_check(&r, Some(ip), &servers, Some("zen.spamhaus.org")).await;
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].server, "zen.spamhaus.org");

        // Unresolved IP → every row is an error row.
        let none = run_blocklist_self_check(&r, None, &servers, None).await;
        assert!(none.iter().all(|x| x.error == "outbound IP unresolved"));
    }
}
