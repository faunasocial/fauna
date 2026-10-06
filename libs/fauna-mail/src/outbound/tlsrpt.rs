//! TLSRPT outbound reporter — docs/goal/behavior/smtp-server.md
//! § TLSRPT outbound reporter (RFC 8460).
//!
//! The MTA aggregates per-recipient-domain outcomes across the
//! delivery day and emits one report per recipient domain that
//! publishes a `_smtp._tls.<domain>` TLSRPT policy. The pure-Rust
//! aggregator + report builder + per-transport envelope builders
//! live here. The wire-level DNS lookup and per-protocol dispatch
//! (mailto: queue re-enqueue / https: POST) is the bridge's
//! concern — this module produces the byte-shaped payloads, the
//! bridge handles the I/O.
//!
//! Implements RFC 8460 §4.4 JSON shape and §5.3 mailto: framing.

use std::collections::HashMap;
use std::io::Write;
use std::sync::Mutex;

use flate2::Compression;
use flate2::write::GzEncoder;
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TlsrptPolicy {
    pub policy_type: String,
    pub policy_string: Vec<String>,
    pub policy_domain: String,
}

#[derive(Debug, Clone)]
pub struct AttemptOutcome {
    pub recipient_domain: String,
    pub policy: TlsrptPolicy,
    /// `None` = successful TLS session; `Some(s)` = failure where `s`
    /// is one of the RFC 8460 §4.3 `result-type` enum values
    /// (`starttls-not-supported`, `certificate-host-mismatch`,
    /// `validation-failure`, `tlsa-invalid`, etc.).
    pub failure_type: Option<String>,
}

#[derive(Debug, Default)]
pub struct TlsrptAggregator {
    /// recipient_domain -> policy -> failure_type-or-None -> count
    buckets: HashMap<String, HashMap<TlsrptPolicy, HashMap<Option<String>, u64>>>,
}

impl TlsrptAggregator {
    pub fn record(&mut self, outcome: AttemptOutcome) {
        let by_policy = self.buckets.entry(outcome.recipient_domain).or_default();
        let by_failure = by_policy.entry(outcome.policy).or_default();
        *by_failure.entry(outcome.failure_type).or_default() += 1;
    }

    /// Build the RFC 8460 §4.4 JSON report envelope for `domain` on
    /// `report_date` (YYYY-MM-DD). Caller fills in
    /// `envelope.transports` from the recipient's TLSRPT policy.
    pub fn emit_report(
        &self,
        domain: &str,
        our_domain: &str,
        report_id: &str,
        report_date: &str,
    ) -> ReportEnvelope {
        let by_policy = self.buckets.get(domain);
        let policies: Vec<Value> = match by_policy {
            Some(map) => map
                .iter()
                .map(|(policy, by_failure)| {
                    let mut successes: u64 = 0;
                    let mut failures: u64 = 0;
                    let mut failure_details: Vec<Value> = Vec::new();
                    let mut by_failure_sorted: Vec<(&Option<String>, &u64)> =
                        by_failure.iter().collect();
                    by_failure_sorted.sort_by(|a, b| a.0.as_ref().cmp(&b.0.as_ref()));
                    for (failure_opt, count) in by_failure_sorted {
                        if let Some(ft) = failure_opt {
                            failures += count;
                            failure_details.push(json!({
                                "result-type": ft,
                                "failed-session-count": *count,
                            }));
                        } else {
                            successes += count;
                        }
                    }
                    json!({
                        "policy": {
                            "policy-type": policy.policy_type,
                            "policy-string": policy.policy_string,
                            "policy-domain": policy.policy_domain,
                        },
                        "summary": {
                            "total-successful-session-count": successes,
                            "total-failure-session-count": failures,
                        },
                        "failure-details": failure_details,
                    })
                })
                .collect(),
            None => Vec::new(),
        };

        let report = json!({
            "organization-name": our_domain,
            "date-range": {
                "start-datetime": format!("{report_date}T00:00:00Z"),
                "end-datetime": format!("{report_date}T23:59:59Z"),
            },
            "contact-info": format!("postmaster@{our_domain}"),
            "report-id": report_id,
            "policies": policies,
        });

        let json_bytes = serde_json::to_vec(&report).expect("serialize TLSRPT report");

        ReportEnvelope {
            json: json_bytes,
            transports: Vec::new(),
        }
    }

    pub fn clear_domain(&mut self, domain: &str) {
        self.buckets.remove(domain);
    }

    pub fn clear(&mut self) {
        self.buckets.clear();
    }

    pub fn recorded_domains(&self) -> Vec<String> {
        self.buckets.keys().cloned().collect()
    }

    /// Deterministically-ordered snapshot of the aggregator's buckets,
    /// suitable for JSON serialization by the test-only `/api/v1/test/
    /// outbound/tlsrpt_aggregator_dump` endpoint. Sort order: by
    /// `recipient_domain`, then `policy_type`, then `policy_domain`,
    /// then failure `result_type`. The production daily emitter consumes
    /// the same `HashMap` via `emit_report`; this method exists purely
    /// so tests can assert on the raw bucket state without round-tripping
    /// through the RFC 8460 JSON envelope.
    pub fn dump_snapshot(&self) -> Vec<DomainSnapshot> {
        let mut domains: Vec<&String> = self.buckets.keys().collect();
        domains.sort();
        domains
            .into_iter()
            .map(|domain| {
                let by_policy = &self.buckets[domain];
                let mut policies: Vec<(&TlsrptPolicy, &HashMap<Option<String>, u64>)> =
                    by_policy.iter().collect();
                policies.sort_by(|a, b| {
                    a.0.policy_type
                        .cmp(&b.0.policy_type)
                        .then_with(|| a.0.policy_domain.cmp(&b.0.policy_domain))
                });
                let policies = policies
                    .into_iter()
                    .map(|(policy, by_failure)| {
                        let mut total_success: u64 = 0;
                        let mut total_failure: u64 = 0;
                        let mut failures: Vec<FailureSnapshot> = by_failure
                            .iter()
                            .filter_map(|(failure, count)| match failure {
                                Some(rt) => {
                                    total_failure += count;
                                    Some(FailureSnapshot {
                                        result_type: rt.clone(),
                                        count: *count,
                                    })
                                }
                                None => {
                                    total_success += count;
                                    None
                                }
                            })
                            .collect();
                        failures.sort_by(|a, b| a.result_type.cmp(&b.result_type));
                        PolicySnapshot {
                            policy_type: policy.policy_type.clone(),
                            policy_string: policy.policy_string.clone(),
                            policy_domain: policy.policy_domain.clone(),
                            total_success,
                            total_failure,
                            failures,
                        }
                    })
                    .collect();
                DomainSnapshot {
                    domain: domain.clone(),
                    policies,
                }
            })
            .collect()
    }
}

/// Bucket snapshot for one recipient domain — see
/// `TlsrptAggregator::dump_snapshot`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DomainSnapshot {
    pub domain: String,
    pub policies: Vec<PolicySnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PolicySnapshot {
    pub policy_type: String,
    pub policy_string: Vec<String>,
    pub policy_domain: String,
    pub total_success: u64,
    pub total_failure: u64,
    pub failures: Vec<FailureSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FailureSnapshot {
    pub result_type: String,
    pub count: u64,
}

/// Hook the outbound delivery path holds onto for recording per-attempt
/// TLS verdicts. `legacy_smtp_outbound::spawn` hands the same
/// `Arc<Mutex<TlsrptAggregator>>` to both the send-fn closure and the
/// daily-dispatch task so the per-attempt records the closure pushes are
/// the same records the daily emitter iterates at 00:00 UTC.
pub trait OutboundTlsrptRecorder: Send + Sync {
    fn record(&self, outcome: AttemptOutcome);
}

/// Production impl. Lock contention is negligible: the daily emitter
/// only touches the lock once per recipient-domain at 00:00 UTC, and the
/// delivery path holds it for the duration of one `HashMap` insert.
impl OutboundTlsrptRecorder for std::sync::Mutex<TlsrptAggregator> {
    fn record(&self, outcome: AttemptOutcome) {
        // Poisoning here would mean a previous record panicked while
        // holding the lock — fall back to the inner state via the
        // poison API so one corrupt record doesn't sink the whole
        // outbound queue.
        let mut guard = match self.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.record(outcome);
    }
}

/// No-op recorder for tests / callers that don't yet plug into a real
/// aggregator. `deliver_raw`'s recorder argument is non-optional by
/// design (no production caller can skip TLSRPT) — this is the test
/// stand-in.
pub struct NullOutboundTlsrptRecorder;

impl OutboundTlsrptRecorder for NullOutboundTlsrptRecorder {
    fn record(&self, _outcome: AttemptOutcome) {}
}

/// Build the [`TlsrptPolicy`] for one TLS attempt to `mx_host` while
/// delivering to `recipient_domain`.
///
/// `tlsa_policy_strings` is the list of RFC 8460 §4.4 policy-string
/// entries the DANE verifier was pinned to (`"usage selector matching
/// hex"`). `mta_sts` is the result of the MTA-STS lookup (see
/// [`crate::outbound::mta_sts`]). Precedence per RFC 8460 §4.3:
/// `tlsa` > `sts` (any `Found` / `FetchError` / `Invalid`) >
/// `no-policy-found`. `Found` contributes its
/// `MtaStsPolicy::policy_strings()`; `FetchError` / `Invalid` attribute
/// `policy_type="sts"` with empty `policy_string` (the failure
/// result-type is set per-attempt by the caller).
pub fn policy_for_attempt(
    recipient_domain: &str,
    mx_host: &str,
    tlsa_policy_strings: &[String],
    mta_sts: &crate::outbound::mta_sts::MtaStsLookup,
) -> TlsrptPolicy {
    use crate::outbound::mta_sts::MtaStsLookup;

    if !tlsa_policy_strings.is_empty() {
        return TlsrptPolicy {
            policy_type: "tlsa".to_string(),
            policy_string: tlsa_policy_strings.to_vec(),
            policy_domain: mx_host.to_string(),
        };
    }
    match mta_sts {
        MtaStsLookup::Found(fp) => TlsrptPolicy {
            policy_type: "sts".to_string(),
            policy_string: fp.policy.policy_strings(),
            policy_domain: recipient_domain.to_string(),
        },
        MtaStsLookup::FetchError | MtaStsLookup::Invalid => TlsrptPolicy {
            policy_type: "sts".to_string(),
            policy_string: Vec::new(),
            policy_domain: recipient_domain.to_string(),
        },
        MtaStsLookup::NotPublished => TlsrptPolicy {
            policy_type: "no-policy-found".to_string(),
            policy_string: Vec::new(),
            policy_domain: recipient_domain.to_string(),
        },
    }
}

#[derive(Debug, Clone)]
pub struct ReportEnvelope {
    pub json: Vec<u8>,
    pub transports: Vec<ReportTransport>,
}

#[derive(Debug, Clone)]
pub enum ReportTransport {
    Mailto {
        rcpt: String,
        mime_message: Vec<u8>,
    },
    Https {
        uri: String,
        body: Vec<u8>,
        content_encoding: &'static str,
    },
}

/// Parse `rua=...` URIs from a `_smtp._tls.<domain>` TXT record per
/// RFC 8460 §3.
pub fn parse_rua_uris(txt: &str) -> Option<Vec<String>> {
    if !txt.contains("v=TLSRPTv1") {
        return None;
    }
    let rua = txt
        .split(';')
        .map(str::trim)
        .find_map(|tag| tag.strip_prefix("rua="))?;
    Some(
        rua.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
    )
}

/// Resolves a recipient domain's TLSRPT policy at daily-emit time, returning
/// its `rua=` URI list (`Some(uris)`) or `None` if the domain publishes no
/// `_smtp._tls.<domain>` TLSRPT policy. Mirrors the
/// [`crate::outbound::mta_sts::MtaStsFetcher`] boundary so the daily-emit
/// task can mock the lookup without touching the network. Unlike MTA-STS,
/// this is consulted once per recipient-domain per day (at emit), not per
/// delivery — so [`CachingTlsrptPolicyFetcher`] is a plain fixed-TTL cache
/// with no per-delivery singleflight machinery.
#[async_trait::async_trait]
pub trait TlsrptPolicyFetcher: Send + Sync {
    async fn lookup(&self, domain: &str) -> anyhow::Result<Option<Vec<String>>>;
}

/// Fetcher that always reports "no TLSRPT policy". Test fixtures + the
/// fallback when the live resolver fails to build.
pub struct NullTlsrptPolicyFetcher;

#[async_trait::async_trait]
impl TlsrptPolicyFetcher for NullTlsrptPolicyFetcher {
    async fn lookup(&self, _domain: &str) -> anyhow::Result<Option<Vec<String>>> {
        Ok(None)
    }
}

/// `_smtp._tls.<domain>` TXT cache TTL. RFC 8460 §3 TLSRPT records carry no
/// `max_age`, so we apply a fixed 24 h per smtp-server.md § TLSRPT outbound
/// reporter ("Fetch the recipient's TLSRPT policy at the start of each
/// delivery day (cached for 24 h)"). Negative results (no policy) cache for
/// the same window.
pub const TLSRPT_POLICY_CACHE_TTL_SECS: i64 = 24 * 60 * 60;

struct TlsrptCacheEntry {
    rua: Option<Vec<String>>,
    fetched_at: i64,
}

/// Fixed-24 h in-memory cache wrapping any [`TlsrptPolicyFetcher`]. The
/// daily emitter fetches each recorded recipient domain's policy once per
/// run; the cache spans runs so a domain whose policy was just read isn't
/// re-queried if the emit is retried within the day.
pub struct CachingTlsrptPolicyFetcher<F: TlsrptPolicyFetcher> {
    inner: F,
    clock: crate::outbound::mta_sts::ClockFn,
    cache: Mutex<HashMap<String, TlsrptCacheEntry>>,
}

impl<F: TlsrptPolicyFetcher> CachingTlsrptPolicyFetcher<F> {
    pub fn new(inner: F, clock: crate::outbound::mta_sts::ClockFn) -> Self {
        Self {
            inner,
            clock,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Accessor for the wrapped fetcher — tests assert on call counters.
    pub fn inner(&self) -> &F {
        &self.inner
    }

    fn cached(&self, domain: &str, now: i64) -> Option<Option<Vec<String>>> {
        let cache = self.cache.lock().ok()?;
        let entry = cache.get(domain)?;
        if now.saturating_sub(entry.fetched_at) < TLSRPT_POLICY_CACHE_TTL_SECS {
            Some(entry.rua.clone())
        } else {
            None
        }
    }

    fn insert(&self, domain: &str, rua: &Option<Vec<String>>, now: i64) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(
                domain.to_string(),
                TlsrptCacheEntry {
                    rua: rua.clone(),
                    fetched_at: now,
                },
            );
        }
    }
}

#[async_trait::async_trait]
impl<F: TlsrptPolicyFetcher> TlsrptPolicyFetcher for CachingTlsrptPolicyFetcher<F> {
    async fn lookup(&self, domain: &str) -> anyhow::Result<Option<Vec<String>>> {
        let now = (self.clock)();
        if let Some(hit) = self.cached(domain, now) {
            return Ok(hit);
        }
        let fresh = self.inner.lookup(domain).await?;
        self.insert(domain, &fresh, now);
        Ok(fresh)
    }
}

/// Live `TlsrptPolicyFetcher` over `hickory-resolver`: looks up the
/// `_smtp._tls.<domain>` TXT record (RFC 8460 §3) and extracts its `rua=`
/// URI list via [`parse_rua_uris`]. Native-only (`outbound-net`): mirrors
/// [`crate::outbound::mta_sts::LiveMtaStsFetcher`], lifted out of the retired
/// `fauna-bridge-smtp` crate so the permanent Go-bridge → nest path doesn't
/// couple to it.
#[cfg(feature = "outbound-net")]
pub struct LiveTlsrptPolicyFetcher {
    inner: hickory_resolver::TokioResolver,
}

#[cfg(feature = "outbound-net")]
impl LiveTlsrptPolicyFetcher {
    pub fn new() -> anyhow::Result<Self> {
        let inner = crate::outbound::build_hickory_resolver()?;
        Ok(Self { inner })
    }
}

#[cfg(feature = "outbound-net")]
#[async_trait::async_trait]
impl TlsrptPolicyFetcher for LiveTlsrptPolicyFetcher {
    async fn lookup(&self, domain: &str) -> anyhow::Result<Option<Vec<String>>> {
        let query = format!("_smtp._tls.{domain}");
        let resp = match self.inner.txt_lookup(&query).await {
            Ok(r) => r,
            // No `_smtp._tls.<domain>` TXT — no TLSRPT policy advertised.
            Err(_) => return Ok(None),
        };
        // hickory 0.26: `txt_lookup` returns a generic `Lookup`; records come via
        // `answers()` and the TXT rdata is extracted by matching the `RData` enum.
        for rec in resp.answers() {
            let hickory_resolver::proto::rr::RData::TXT(txt) = &rec.data else {
                continue;
            };
            let mut joined = String::new();
            for data in txt.txt_data.iter() {
                joined.push_str(&String::from_utf8_lossy(data));
            }
            if let Some(uris) = parse_rua_uris(&joined) {
                return Ok(Some(uris));
            }
        }
        Ok(None)
    }
}

/// Pick a per-domain 0..`window_secs` jitter offset so the daily emit
/// at 00:00 UTC doesn't stampede all reports out at the same second.
pub fn sample_jitter_offset_secs(window_secs: u32) -> i64 {
    use rand::Rng;
    let max = window_secs.max(1);
    rand::thread_rng().gen_range(0..max) as i64
}

/// Seconds from `now` (epoch) until the next 00:00:00 UTC. When `now` is
/// exactly midnight, returns 86400 — the daily emitter never fires twice in
/// the same UTC day.
pub fn seconds_until_next_utc_midnight(now: i64) -> u32 {
    let day = 86_400i64;
    let into_day = ((now % day) + day) % day;
    (day - into_day) as u32
}

/// Howard Hinnant's `civil_from_days` (public domain) — epoch-days → (year,
/// month, day). <http://howardhinnant.github.io/date_algorithms.html>
fn days_to_ymd(days_since_epoch: i64) -> (i32, u32, u32) {
    fauna_core::caltime::civil_from_days(days_since_epoch)
}

/// Format `epoch` as `YYYY-MM-DD` (UTC) — the TLSRPT `date-range` /
/// report-date / Subject components.
pub fn format_utc_date(epoch: i64) -> String {
    let (year, month, day) = days_to_ymd(epoch.div_euclid(86_400));
    format!("{year:04}-{month:02}-{day:02}")
}

/// Format `epoch` as an RFC 5322 `Date:` header value in UTC, e.g.
/// `Tue, 23 May 2026 00:00:00 +0000`. Used for the `mailto:` report MIME's
/// `Date:` header (the recipient MTA carries it verbatim).
pub fn rfc2822_utc(epoch: i64) -> String {
    fauna_core::imf_date::format_rfc5322_date(epoch)
}

/// Per-report context shared across `populate_transports` and
/// [`build_mailto_mime`]. Carried as one struct so callers (queue
/// dispatch task, test endpoint) supply non-pure inputs (date, uuid,
/// boundary) in one place — keeps `build_mailto_mime` itself a pure
/// function over byte slices.
#[derive(Debug, Clone, Copy)]
pub struct ReportDispatchContext<'a> {
    pub our_domain: &'a str,
    pub recipient_domain: &'a str,
    pub report_id: &'a str,
    /// `YYYY-MM-DD` UTC — used both in the Subject header and in the
    /// RFC 8460 §5.3 attachment filename convention.
    pub report_date: &'a str,
    /// Local-part of the `Message-ID:` header; the domain is
    /// `our_domain`.
    pub message_id: &'a str,
    /// RFC 2822 `Date:` header value (the live dispatch task formats
    /// `now`; the test endpoint passes a fixed value).
    pub rfc2822_date: &'a str,
    /// MIME multipart boundary token. Must not appear in the body.
    pub boundary: &'a str,
}

/// Gzip the report JSON in one shot. Used for both the `mailto:`
/// attachment payload and the `https:` POST body.
pub fn gzip_bytes(input: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(input)
        .expect("gzip into Vec is infallible");
    encoder
        .finish()
        .expect("gzip finish into Vec is infallible")
}

/// Build the RFC 8460 §5.3 `multipart/report; report-type="tlsrpt"`
/// MIME envelope for a `mailto:` recipient. The gzipped report is the
/// attached `application/tlsrpt+gzip` part, base64-encoded so the
/// envelope is 7-bit-safe under SMTP. The single text part is the
/// human-readable explainer.
///
/// Caller wraps non-pure inputs (now, uuid, boundary) into `ctx` so
/// this function stays a pure byte transform over `gzipped_json`.
pub fn build_mailto_mime(
    rcpt: &str,
    ctx: &ReportDispatchContext<'_>,
    gzipped_json: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(gzipped_json.len() * 2 + 1024);

    // Headers (RFC 5322 §2.2 + RFC 8460 §5.3).
    let _ = writeln_crlf(&mut out, &format!("From: tlsrpt@{}", ctx.our_domain));
    let _ = writeln_crlf(&mut out, &format!("To: {rcpt}"));
    let _ = writeln_crlf(
        &mut out,
        &format!(
            "Subject: Report Domain: {} Submitter: {} Report-ID: {}",
            ctx.recipient_domain, ctx.our_domain, ctx.report_id
        ),
    );
    let _ = writeln_crlf(
        &mut out,
        &format!("Message-ID: <{}@{}>", ctx.message_id, ctx.our_domain),
    );
    let _ = writeln_crlf(&mut out, &format!("Date: {}", ctx.rfc2822_date));
    let _ = writeln_crlf(&mut out, "MIME-Version: 1.0");
    let _ = writeln_crlf(&mut out, "Auto-Submitted: auto-generated");
    let _ = writeln_crlf(
        &mut out,
        &format!("TLS-Report-Domain: {}", ctx.recipient_domain),
    );
    let _ = writeln_crlf(
        &mut out,
        &format!("TLS-Report-Submitter: {}", ctx.our_domain),
    );
    let _ = writeln_crlf(
        &mut out,
        &format!(
            r#"Content-Type: multipart/report; report-type="tlsrpt"; boundary="{}""#,
            ctx.boundary
        ),
    );
    let _ = writeln_crlf(&mut out, "");

    // Text part — short human explainer so the message renders in a
    // MUA that doesn't open the attachment.
    let _ = writeln_crlf(&mut out, &format!("--{}", ctx.boundary));
    let _ = writeln_crlf(&mut out, "Content-Type: text/plain; charset=utf-8");
    let _ = writeln_crlf(&mut out, "");
    let _ = writeln_crlf(
        &mut out,
        &format!(
            "This is a TLSRPT aggregate report from {} covering delivery attempts to {} on {} (RFC 8460).",
            ctx.our_domain, ctx.recipient_domain, ctx.report_date
        ),
    );
    let _ = writeln_crlf(&mut out, "");

    // Report part — gzipped JSON, base64-encoded for 7-bit SMTP
    // transport.
    let _ = writeln_crlf(&mut out, &format!("--{}", ctx.boundary));
    let filename = mailto_filename(ctx);
    let _ = writeln_crlf(&mut out, "Content-Type: application/tlsrpt+gzip");
    let _ = writeln_crlf(
        &mut out,
        &format!(r#"Content-Disposition: attachment; filename="{filename}""#),
    );
    let _ = writeln_crlf(&mut out, "Content-Transfer-Encoding: base64");
    let _ = writeln_crlf(&mut out, "");
    out.extend_from_slice(fauna_core::mime_wrap::base64_wrap_76(gzipped_json).as_bytes());
    let _ = writeln_crlf(&mut out, "");
    let _ = writeln_crlf(&mut out, &format!("--{}--", ctx.boundary));

    out
}

/// Populate `envelope.transports` from the recipient's `rua=` URI list.
/// `mailto:` URIs produce a [`ReportTransport::Mailto`] with the
/// RFC 8460 §5.3 MIME envelope; `https:` URIs produce a
/// [`ReportTransport::Https`] carrying the gzipped JSON body verbatim.
/// Other schemes (`http://`, `ftp://`, etc.) are skipped silently —
/// RFC 8460 §3.1 only blesses `mailto` and `https`.
pub fn populate_transports(
    envelope: &mut ReportEnvelope,
    rua_uris: &[String],
    ctx: &ReportDispatchContext<'_>,
) {
    let gzipped = gzip_bytes(&envelope.json);
    for uri in rua_uris {
        if let Some(rest) = uri.strip_prefix("mailto:") {
            // Strip optional `?subject=...&body=...` query (RFC 6068)
            // — we generate our own headers.
            let rcpt = rest.split('?').next().unwrap_or("").to_string();
            if rcpt.is_empty() {
                continue;
            }
            let mime = build_mailto_mime(&rcpt, ctx, &gzipped);
            envelope.transports.push(ReportTransport::Mailto {
                rcpt,
                mime_message: mime,
            });
        } else if uri.starts_with("https://") {
            envelope.transports.push(ReportTransport::Https {
                uri: uri.clone(),
                body: gzipped.clone(),
                content_encoding: "gzip",
            });
        }
        // else: skip per RFC 8460 §3.1.
    }
}

/// RFC 8460 §5.3 filename convention:
/// `<submitter>!<recipient>!<start-secs>!<end-secs>!<report-id>.json.gz`.
fn mailto_filename(ctx: &ReportDispatchContext<'_>) -> String {
    format!(
        "{}!{}!{}!{}!{}.json.gz",
        ctx.our_domain, ctx.recipient_domain, ctx.report_date, ctx.report_date, ctx.report_id,
    )
}

fn writeln_crlf(out: &mut Vec<u8>, line: &str) -> std::io::Result<()> {
    out.extend_from_slice(line.as_bytes());
    out.extend_from_slice(b"\r\n");
    Ok(())
}

/// HTTP surface for the TLSRPT daily emitter — POSTs the gzipped JSON report
/// to an `https:` rua URI with `Content-Type: application/tlsrpt+gzip` +
/// `Content-Encoding: gzip` (RFC 8460 §3.2). Returns the response status
/// code; the daily emitter treats non-2xx as a non-fatal "don't persist this
/// transport". A trait so the daily-emit task is testable without real HTTP.
#[async_trait::async_trait]
pub trait TlsrptHttpPoster: Send + Sync {
    async fn post_tlsrpt(&self, uri: &str, gzipped_body: Vec<u8>) -> anyhow::Result<u16>;
}

/// No-op poster for tests — records nothing, always reports 200. Lets the
/// daily-emit dispatch be exercised without standing up an HTTP server.
pub struct NullTlsrptHttpPoster;

#[async_trait::async_trait]
impl TlsrptHttpPoster for NullTlsrptHttpPoster {
    async fn post_tlsrpt(&self, _uri: &str, _gzipped_body: Vec<u8>) -> anyhow::Result<u16> {
        Ok(200)
    }
}

/// Production `reqwest`-backed poster (native-only: `outbound-net`). Lifted
/// out of the retired `fauna-bridge-smtp` crate so the permanent nest-side
/// daily emitter doesn't couple to it.
#[cfg(feature = "outbound-net")]
pub struct ReqwestTlsrptPoster {
    client: reqwest::Client,
}

#[cfg(feature = "outbound-net")]
impl ReqwestTlsrptPoster {
    pub fn new() -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        Ok(Self { client })
    }
}

#[cfg(feature = "outbound-net")]
impl Default for ReqwestTlsrptPoster {
    fn default() -> Self {
        Self::new().expect("build reqwest client for TLSRPT poster")
    }
}

#[cfg(feature = "outbound-net")]
#[async_trait::async_trait]
impl TlsrptHttpPoster for ReqwestTlsrptPoster {
    async fn post_tlsrpt(&self, uri: &str, gzipped_body: Vec<u8>) -> anyhow::Result<u16> {
        let resp = self
            .client
            .post(uri)
            .header("Content-Type", "application/tlsrpt+gzip")
            .header("Content-Encoding", "gzip")
            .body(gzipped_body)
            .send()
            .await?;
        Ok(resp.status().as_u16())
    }
}
