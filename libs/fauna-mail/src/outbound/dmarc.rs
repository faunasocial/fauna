//! DMARC aggregate-report outbound sender — docs/goal/behavior/dmarc-reporting.md
//! § Outbound aggregate-report sender (RFC 7489 §7.2).
//!
//! The mirror of [`super::tlsrpt`]: the MTA reports one
//! [`DmarcObservation`] per inbound message it verified (before any
//! enforcement gate, so refused mail counts too), the nest buckets it here in a
//! [`DmarcAggregator`], and once per UTC day it resolves each reported From
//! domain's DMARC policy ([`discover_policy`]), keeps the `rua=` destinations
//! it may send to ([`plan_destinations`], [`external_destination_authorized`]),
//! serializes the RFC 7489 §A.1 XML ([`build_aggregate_xml`]), splits it under
//! the size cap ([`build_reports`]) and frames each part as a mail
//! ([`build_report_mime`]) for the standard outbound queue.
//!
//! Everything here is pure or behind the [`DmarcRecordFetcher`] trait; the
//! nest supplies the queue, the persistence and the schedule
//! (`bins/fauna-nest/src/outbound_dmarc.rs`).

use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::sync::Mutex;

use fauna_core::mail_auth::{DkimAuthResult, DmarcObservation, DmarcPolicy, DmarcVerdict};

use crate::dmarc_report::Disposition;

/// Our own cap on one report's gzipped size (§ Size cap). A peer's
/// `ruri-size` may lower it per destination, never raise it.
pub const DMARC_REPORT_CAP_BYTES: u64 = 10 * 1024 * 1024;

/// How many `rua=` URIs of a peer's record we honour, in record order (§ step 2).
pub const MAX_RUA_URIS: usize = 2;

/// Most distinct From domains one aggregation window tracks. A message from a
/// domain past the cap is not counted ([`DmarcAggregator::record`] returns
/// `false`), so a flood of throwaway From domains costs bounded memory.
pub const MAX_REPORTED_DOMAINS: usize = 10_000;

/// Most distinct rows one From domain tracks in one window (same reason).
pub const MAX_ROWS_PER_DOMAIN: usize = 10_000;

/// Most `_dmarc` lookups one policy discovery makes (the DMARCbis-style tree
/// walk's bound: the From domain, then each parent while two labels remain).
pub const MAX_POLICY_WALK: usize = 8;

/// `_dmarc` TXT cache lifetime — one day, the report interval: a peer's policy
/// is read once per daily pass (§ step 2).
pub const DMARC_RECORD_CACHE_TTL_SECS: i64 = 24 * 60 * 60;

/// The disposition our MTA applied to a message, from its DMARC verdict and the
/// admin's inbound enforcement switches (`mail-policy-config.md` § DMARC
/// enforcement gates) — the `policy_evaluated/disposition` a report states.
///
/// Mirrors what the MTA does with the same inputs: a `p=reject` failure is
/// refused (550 5.7.1) when DMARC enforcement is on and the stage is not in
/// log-only mode (`internal/mta/auth_enforce.go`); a `p=quarantine` failure goes
/// to the recipient's Junk when quarantine is honoured, which log-only does not
/// switch off (the spam gate's `PolicyJunk`). Anything else was delivered as
/// usual.
pub fn applied_disposition(
    verdict: &DmarcVerdict,
    enforce_dmarc: bool,
    enforce_dmarc_quarantine: bool,
    log_only: bool,
) -> Disposition {
    match verdict {
        DmarcVerdict::Fail {
            policy: DmarcPolicy::Reject,
        } if enforce_dmarc && !log_only => Disposition::Reject,
        DmarcVerdict::Fail {
            policy: DmarcPolicy::Quarantine,
        } if enforce_dmarc_quarantine => Disposition::Quarantine,
        _ => Disposition::None,
    }
}

fn disposition_token(d: Disposition) -> &'static str {
    match d {
        Disposition::None => "none",
        Disposition::Quarantine => "quarantine",
        Disposition::Reject => "reject",
    }
}

// ── Aggregation ──────────────────────────────────────────────────────────────

/// One `<record>` of a report: every message that agreed on all of these
/// fields, counted.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
pub struct ReportRow {
    pub header_from: String,
    pub source_ip: String,
    pub envelope_from: String,
    #[serde(serialize_with = "ser_disposition")]
    pub disposition: DispositionKey,
    pub dkim_aligned: bool,
    pub spf_aligned: bool,
    pub dkim_results: Vec<(String, String, String)>,
    pub spf_result: String,
    pub count: u64,
}

/// [`Disposition`] as an orderable key (the receive-side enum is not `Ord`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DispositionKey {
    None,
    Quarantine,
    Reject,
}

impl From<Disposition> for DispositionKey {
    fn from(d: Disposition) -> Self {
        match d {
            Disposition::None => Self::None,
            Disposition::Quarantine => Self::Quarantine,
            Disposition::Reject => Self::Reject,
        }
    }
}

impl From<DispositionKey> for Disposition {
    fn from(d: DispositionKey) -> Self {
        match d {
            DispositionKey::None => Self::None,
            DispositionKey::Quarantine => Self::Quarantine,
            DispositionKey::Reject => Self::Reject,
        }
    }
}

fn ser_disposition<S: serde::Serializer>(d: &DispositionKey, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(disposition_token((*d).into()))
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RowKey {
    source_ip: String,
    envelope_from: String,
    disposition: DispositionKey,
    dkim_aligned: bool,
    spf_aligned: bool,
    dkim_results: Vec<(String, String, String)>,
    spf_result: String,
}

/// The day's per-From-domain buckets (§ step 1). In memory, like the TLSRPT
/// aggregator: a restart loses at most the open day's counts, which a peer
/// reads as one day without a report from us.
#[derive(Debug, Default)]
pub struct DmarcAggregator {
    domains: HashMap<String, HashMap<RowKey, u64>>,
}

impl DmarcAggregator {
    /// Count one message. Returns `false` (nothing counted) when the message
    /// names no From domain or the window's domain or row cap is reached.
    pub fn record(
        &mut self,
        source_ip: IpAddr,
        obs: &DmarcObservation,
        disposition: Disposition,
    ) -> bool {
        let domain = obs
            .header_from_domain
            .trim_end_matches('.')
            .to_ascii_lowercase();
        if domain.is_empty() {
            return false;
        }
        if !self.domains.contains_key(&domain) && self.domains.len() >= MAX_REPORTED_DOMAINS {
            return false;
        }
        let rows = self.domains.entry(domain).or_default();
        let key = RowKey {
            source_ip: source_ip.to_string(),
            envelope_from: obs.envelope_from_domain.to_ascii_lowercase(),
            disposition: disposition.into(),
            dkim_aligned: obs.dkim_aligned,
            spf_aligned: obs.spf_aligned,
            dkim_results: obs
                .dkim_results
                .iter()
                .map(|r: &DkimAuthResult| (r.domain.clone(), r.selector.clone(), r.result.clone()))
                .collect(),
            spf_result: obs.spf_result.clone(),
        };
        if !rows.contains_key(&key) && rows.len() >= MAX_ROWS_PER_DOMAIN {
            return false;
        }
        let n = rows.entry(key).or_default();
        *n = n.saturating_add(1);
        true
    }

    /// The From domains counted this window, sorted.
    pub fn recorded_domains(&self) -> Vec<String> {
        let mut v: Vec<String> = self.domains.keys().cloned().collect();
        v.sort();
        v
    }

    /// One From domain's rows, sorted (deterministic report bytes).
    pub fn rows(&self, domain: &str) -> Vec<ReportRow> {
        let mut out: Vec<ReportRow> = self
            .domains
            .get(domain)
            .into_iter()
            .flat_map(|rows| rows.iter())
            .map(|(k, n)| ReportRow {
                header_from: domain.to_string(),
                source_ip: k.source_ip.clone(),
                envelope_from: k.envelope_from.clone(),
                disposition: k.disposition,
                dkim_aligned: k.dkim_aligned,
                spf_aligned: k.spf_aligned,
                dkim_results: k.dkim_results.clone(),
                spf_result: k.spf_result.clone(),
                count: *n,
            })
            .collect();
        out.sort();
        out
    }

    /// Close one From domain's window.
    pub fn clear_domain(&mut self, domain: &str) {
        self.domains.remove(domain);
    }

    /// Drop every bucket.
    pub fn clear(&mut self) {
        self.domains.clear();
    }

    /// Every bucket, for the test hook's snapshot.
    pub fn dump_snapshot(&self) -> BTreeMap<String, Vec<ReportRow>> {
        self.recorded_domains()
            .into_iter()
            .map(|d| {
                let rows = self.rows(&d);
                (d, rows)
            })
            .collect()
    }
}

// ── The peer's published record ──────────────────────────────────────────────

/// The tags of a peer's `_dmarc` record a report needs (RFC 7489 §6.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerDmarcRecord {
    /// `p=` — `none`, `quarantine` or `reject`.
    pub p: String,
    /// `sp=`, when published.
    pub sp: Option<String>,
    /// `adkim=` — `r` (the default) or `s`.
    pub adkim: String,
    /// `aspf=` — `r` (the default) or `s`.
    pub aspf: String,
    /// `pct=` — 100 by default.
    pub pct: u8,
    /// `rua=`, split on commas, in record order.
    pub rua: Vec<String>,
}

/// A peer's record and the domain it was found at — the report's
/// `policy_published/domain`, which is the From domain or an ancestor of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerDmarcPolicy {
    pub domain: String,
    pub record: PeerDmarcRecord,
}

/// Parse one `_dmarc` TXT string. `None` when it is not a DMARC record (the
/// first tag must be `v=DMARC1`, RFC 7489 §6.4) or names an unknown policy.
/// A record without `p=` but with a `rua=` is read as `p=none` (§6.6.3).
pub fn parse_dmarc_record(txt: &str) -> Option<PeerDmarcRecord> {
    let mut tags = txt.split(';').map(str::trim).filter(|t| !t.is_empty());
    let first = tags.next()?;
    let (k, v) = first.split_once('=')?;
    if !k.trim().eq_ignore_ascii_case("v") || v.trim() != "DMARC1" {
        return None;
    }
    let mut p = None;
    let mut sp = None;
    let mut adkim = "r".to_string();
    let mut aspf = "r".to_string();
    let mut pct = 100u8;
    let mut rua = Vec::new();
    for tag in tags {
        let Some((k, v)) = tag.split_once('=') else {
            continue;
        };
        let v = v.trim();
        match k.trim().to_ascii_lowercase().as_str() {
            "p" => p = Some(v.to_ascii_lowercase()),
            "sp" => sp = Some(v.to_ascii_lowercase()),
            "adkim" => adkim = v.to_ascii_lowercase(),
            "aspf" => aspf = v.to_ascii_lowercase(),
            "pct" => pct = v.parse::<u8>().map(|n| n.min(100)).unwrap_or(100),
            "rua" => {
                rua = v
                    .split(',')
                    .map(|u| u.trim().to_string())
                    .filter(|u| !u.is_empty())
                    .collect()
            }
            _ => {}
        }
    }
    let p = match p {
        Some(p) => p,
        None if !rua.is_empty() => "none".to_string(),
        None => return None,
    };
    let valid = |s: &str| matches!(s, "none" | "quarantine" | "reject");
    if !valid(&p) {
        return None;
    }
    let sp = sp.filter(|s| valid(s));
    let one_of_rs = |s: String| if s == "s" { s } else { "r".to_string() };
    Some(PeerDmarcRecord {
        p,
        sp,
        adkim: one_of_rs(adkim),
        aspf: one_of_rs(aspf),
        pct,
        rua,
    })
}

/// One `rua=` URI, read (RFC 7489 §6.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuaDestination {
    /// `mailto:<address>[!<size>]` — `max_bytes` is the `ruri-size` limit.
    Mailto {
        address: String,
        max_bytes: Option<u64>,
    },
    /// Any other scheme (`https:` above all) — DMARC reports are mailto-only.
    Unsupported { uri: String },
}

/// Read one `rua=` URI. A `!<n>[k|m|g|t]` suffix is the size limit; an
/// RFC 6068 `?query` on a mailto is dropped (we write our own headers).
pub fn parse_rua(uri: &str) -> RuaDestination {
    let uri = uri.trim();
    let (body, size) = match uri.rsplit_once('!') {
        Some((b, s)) => (b, parse_ruri_size(s)),
        None => (uri, None),
    };
    let lower = body.to_ascii_lowercase();
    if !lower.starts_with("mailto:") {
        return RuaDestination::Unsupported {
            uri: uri.to_string(),
        };
    }
    let address = body["mailto:".len()..]
        .split('?')
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if address.is_empty() || !address.contains('@') {
        return RuaDestination::Unsupported {
            uri: uri.to_string(),
        };
    }
    RuaDestination::Mailto {
        address,
        max_bytes: size,
    }
}

fn parse_ruri_size(s: &str) -> Option<u64> {
    let s = s.trim().to_ascii_lowercase();
    let (digits, mult) = match s.chars().last()? {
        'k' => (&s[..s.len() - 1], 1u64 << 10),
        'm' => (&s[..s.len() - 1], 1u64 << 20),
        'g' => (&s[..s.len() - 1], 1u64 << 30),
        't' => (&s[..s.len() - 1], 1u64 << 40),
        _ => (s.as_str(), 1),
    };
    digits.parse::<u64>().ok()?.checked_mul(mult)
}

/// The destinations one report goes to: the mailto URIs among the first
/// [`MAX_RUA_URIS`] of `rua`, each with its effective size cap
/// (`min(ruri-size, DMARC_REPORT_CAP_BYTES)`), and the count of honoured URIs
/// skipped for their scheme (the `dmarc_outbound_https_uri_skipped_total`
/// counter).
pub fn plan_destinations(rua: &[String]) -> (Vec<(String, u64)>, usize) {
    let mut out = Vec::new();
    let mut skipped = 0;
    for uri in rua.iter().take(MAX_RUA_URIS) {
        match parse_rua(uri) {
            RuaDestination::Mailto { address, max_bytes } => {
                let cap = max_bytes
                    .unwrap_or(DMARC_REPORT_CAP_BYTES)
                    .min(DMARC_REPORT_CAP_BYTES);
                out.push((address, cap));
            }
            RuaDestination::Unsupported { .. } => skipped += 1,
        }
    }
    (out, skipped)
}

// ── DNS ──────────────────────────────────────────────────────────────────────

/// TXT lookups for the sender: the `_dmarc` policy walk and the RFC 7489 §7.1
/// external-destination check. Returns each TXT record's strings joined; an
/// empty list when the name has none. A trait so the daily pass is testable
/// without DNS.
#[async_trait::async_trait]
pub trait DmarcRecordFetcher: Send + Sync {
    async fn txt(&self, name: &str) -> anyhow::Result<Vec<String>>;
}

/// Fetcher that finds nothing — no peer publishes a policy, so nothing is
/// sent. The fallback when the live resolver fails to build.
pub struct NullDmarcRecordFetcher;

#[async_trait::async_trait]
impl DmarcRecordFetcher for NullDmarcRecordFetcher {
    async fn txt(&self, _name: &str) -> anyhow::Result<Vec<String>> {
        Ok(Vec::new())
    }
}

/// Find the DMARC policy that governs `from_domain`: `_dmarc.<from_domain>`,
/// then each parent domain while at least two labels remain, at most
/// [`MAX_POLICY_WALK`] lookups. Walking parents stands in for RFC 7489's
/// organizational-domain fallback without a public-suffix list; it never goes
/// above a two-label name, so it never asks a TLD.
pub async fn discover_policy(
    fetcher: &dyn DmarcRecordFetcher,
    from_domain: &str,
) -> anyhow::Result<Option<PeerDmarcPolicy>> {
    let mut domain = from_domain.trim_end_matches('.').to_ascii_lowercase();
    for _ in 0..MAX_POLICY_WALK {
        if domain.split('.').filter(|l| !l.is_empty()).count() < 2 {
            break;
        }
        let records: Vec<PeerDmarcRecord> = fetcher
            .txt(&format!("_dmarc.{domain}"))
            .await?
            .iter()
            .filter_map(|t| parse_dmarc_record(t))
            .collect();
        // RFC 7489 §6.6.3: more than one DMARC record at a name is no record.
        if records.len() == 1 {
            return Ok(Some(PeerDmarcPolicy {
                domain,
                record: records.into_iter().next().expect("len checked"),
            }));
        }
        match domain.split_once('.') {
            Some((_, parent)) => domain = parent.to_string(),
            None => break,
        }
    }
    Ok(None)
}

/// May a report about `policy_domain` go to a mailbox at `rua_domain`
/// (RFC 7489 §7.1)? Yes without asking when the two are the same domain or one
/// is beneath the other; otherwise only when the destination publishes
/// `<policy_domain>._report._dmarc.<rua_domain>` with a `v=DMARC1` record. A
/// lookup failure is a no — a report goes nowhere it was not invited.
pub async fn external_destination_authorized(
    fetcher: &dyn DmarcRecordFetcher,
    policy_domain: &str,
    rua_domain: &str,
) -> bool {
    let p = policy_domain.trim_end_matches('.').to_ascii_lowercase();
    let r = rua_domain.trim_end_matches('.').to_ascii_lowercase();
    if p == r || p.ends_with(&format!(".{r}")) || r.ends_with(&format!(".{p}")) {
        return true;
    }
    match fetcher.txt(&format!("{p}._report._dmarc.{r}")).await {
        Ok(txts) => txts.iter().any(|t| {
            t.split(';')
                .next()
                .and_then(|tag| tag.split_once('='))
                .is_some_and(|(k, v)| k.trim().eq_ignore_ascii_case("v") && v.trim() == "DMARC1")
        }),
        Err(_) => false,
    }
}

struct CacheEntry {
    txt: Vec<String>,
    fetched_at: i64,
}

/// Fixed-TTL cache over any [`DmarcRecordFetcher`] (errors are not cached).
pub struct CachingDmarcRecordFetcher<F: DmarcRecordFetcher> {
    inner: F,
    clock: crate::outbound::mta_sts::ClockFn,
    cache: Mutex<HashMap<String, CacheEntry>>,
}

impl<F: DmarcRecordFetcher> CachingDmarcRecordFetcher<F> {
    pub fn new(inner: F, clock: crate::outbound::mta_sts::ClockFn) -> Self {
        Self {
            inner,
            clock,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// The wrapped fetcher — tests assert on its call counts.
    pub fn inner(&self) -> &F {
        &self.inner
    }
}

#[async_trait::async_trait]
impl<F: DmarcRecordFetcher> DmarcRecordFetcher for CachingDmarcRecordFetcher<F> {
    async fn txt(&self, name: &str) -> anyhow::Result<Vec<String>> {
        let now = (self.clock)();
        let key = name.to_ascii_lowercase();
        if let Ok(cache) = self.cache.lock()
            && let Some(e) = cache.get(&key)
            && now.saturating_sub(e.fetched_at) < DMARC_RECORD_CACHE_TTL_SECS
        {
            return Ok(e.txt.clone());
        }
        let fresh = self.inner.txt(name).await?;
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(
                key,
                CacheEntry {
                    txt: fresh.clone(),
                    fetched_at: now,
                },
            );
        }
        Ok(fresh)
    }
}

/// Live [`DmarcRecordFetcher`] over `hickory-resolver` (native only). A name
/// with no TXT, or a failed lookup, reads as no records — the
/// [`super::tlsrpt::LiveTlsrptPolicyFetcher`] posture: a day whose lookup
/// fails sends no report rather than a wrong one.
#[cfg(feature = "outbound-net")]
pub struct LiveDmarcRecordFetcher {
    inner: hickory_resolver::TokioResolver,
}

#[cfg(feature = "outbound-net")]
impl LiveDmarcRecordFetcher {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            inner: crate::outbound::build_hickory_resolver()?,
        })
    }
}

#[cfg(feature = "outbound-net")]
#[async_trait::async_trait]
impl DmarcRecordFetcher for LiveDmarcRecordFetcher {
    async fn txt(&self, name: &str) -> anyhow::Result<Vec<String>> {
        let resp = match self.inner.txt_lookup(name).await {
            Ok(r) => r,
            Err(_) => return Ok(Vec::new()),
        };
        let mut out = Vec::new();
        for rec in resp.answers() {
            let hickory_resolver::proto::rr::RData::TXT(txt) = &rec.data else {
                continue;
            };
            let mut joined = String::new();
            for data in txt.txt_data.iter() {
                joined.push_str(&String::from_utf8_lossy(data));
            }
            out.push(joined);
        }
        Ok(out)
    }
}

// ── The report ───────────────────────────────────────────────────────────────

/// `report_metadata` of one report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportMeta {
    /// Our primary mail domain — `org_name`, and the `Submitter:` of the mail.
    pub org_name: String,
    /// `dmarc-report@<primary>` — `email`, and the mail's `From:`.
    pub email: String,
    pub report_id: String,
    /// The window, epoch seconds (`date_range/begin`, `/end`).
    pub begin: i64,
    pub end: i64,
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if (c as u32) < 0x20 && c != '\t' && c != '\n' && c != '\r' => {}
            c => out.push(c),
        }
    }
    out
}

fn el(out: &mut String, indent: usize, name: &str, value: &str) {
    for _ in 0..indent {
        out.push_str("  ");
    }
    out.push('<');
    out.push_str(name);
    out.push('>');
    out.push_str(&xml_escape(value));
    out.push_str("</");
    out.push_str(name);
    out.push_str(">\n");
}

fn open(out: &mut String, indent: usize, name: &str) {
    for _ in 0..indent {
        out.push_str("  ");
    }
    out.push('<');
    out.push_str(name);
    out.push_str(">\n");
}

fn close(out: &mut String, indent: usize, name: &str) {
    for _ in 0..indent {
        out.push_str("  ");
    }
    out.push_str("</");
    out.push_str(name);
    out.push_str(">\n");
}

/// Serialize one aggregate report as RFC 7489 §A.1 XML.
pub fn build_aggregate_xml(
    meta: &ReportMeta,
    policy: &PeerDmarcPolicy,
    rows: &[ReportRow],
) -> String {
    let mut x = String::with_capacity(512 + rows.len() * 600);
    x.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    open(&mut x, 0, "feedback");
    el(&mut x, 1, "version", "1.0");
    open(&mut x, 1, "report_metadata");
    el(&mut x, 2, "org_name", &meta.org_name);
    el(&mut x, 2, "email", &meta.email);
    el(&mut x, 2, "report_id", &meta.report_id);
    open(&mut x, 2, "date_range");
    el(&mut x, 3, "begin", &meta.begin.to_string());
    el(&mut x, 3, "end", &meta.end.to_string());
    close(&mut x, 2, "date_range");
    close(&mut x, 1, "report_metadata");
    open(&mut x, 1, "policy_published");
    el(&mut x, 2, "domain", &policy.domain);
    el(&mut x, 2, "adkim", &policy.record.adkim);
    el(&mut x, 2, "aspf", &policy.record.aspf);
    el(&mut x, 2, "p", &policy.record.p);
    el(
        &mut x,
        2,
        "sp",
        policy.record.sp.as_deref().unwrap_or(&policy.record.p),
    );
    el(&mut x, 2, "pct", &policy.record.pct.to_string());
    close(&mut x, 1, "policy_published");
    let pass_fail = |b: bool| if b { "pass" } else { "fail" };
    for r in rows {
        open(&mut x, 1, "record");
        open(&mut x, 2, "row");
        el(&mut x, 3, "source_ip", &r.source_ip);
        el(&mut x, 3, "count", &r.count.to_string());
        open(&mut x, 3, "policy_evaluated");
        el(
            &mut x,
            4,
            "disposition",
            disposition_token(r.disposition.into()),
        );
        el(&mut x, 4, "dkim", pass_fail(r.dkim_aligned));
        el(&mut x, 4, "spf", pass_fail(r.spf_aligned));
        close(&mut x, 3, "policy_evaluated");
        close(&mut x, 2, "row");
        open(&mut x, 2, "identifiers");
        el(&mut x, 3, "header_from", &r.header_from);
        close(&mut x, 2, "identifiers");
        open(&mut x, 2, "auth_results");
        for (domain, selector, result) in &r.dkim_results {
            open(&mut x, 3, "dkim");
            el(&mut x, 4, "domain", domain);
            el(&mut x, 4, "selector", selector);
            el(&mut x, 4, "result", result);
            close(&mut x, 3, "dkim");
        }
        open(&mut x, 3, "spf");
        el(&mut x, 4, "domain", &r.envelope_from);
        el(&mut x, 4, "scope", "mfrom");
        el(&mut x, 4, "result", &r.spf_result);
        close(&mut x, 3, "spf");
        close(&mut x, 2, "auth_results");
        close(&mut x, 1, "record");
    }
    close(&mut x, 0, "feedback");
    x
}

/// One report ready to send: its id, the XML, and the gzipped XML that is both
/// the attachment and the `payload_xml_gz` archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltReport {
    pub report_id: String,
    pub xml: String,
    pub xml_gz: Vec<u8>,
}

/// Build the report(s) for one policy domain under `cap_bytes` of gzipped
/// XML (§ Size cap). Rows are ordered by From domain, so a split falls between
/// From domains wherever it can; a part still over the cap is halved again.
/// When the rows fit one report its id is `meta.report_id`; split parts are
/// `<report_id>.1`, `.2`, … A single row that cannot fit alone is dropped —
/// only a destination whose `ruri-size` is below one row's size meets that.
pub fn build_reports(
    meta: &ReportMeta,
    policy: &PeerDmarcPolicy,
    rows: &[ReportRow],
    cap_bytes: u64,
) -> Vec<BuiltReport> {
    let mut sorted = rows.to_vec();
    sorted.sort();
    let chunks = collect_chunks(meta, policy, &sorted, cap_bytes);
    let n = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(i, chunk)| {
            let report_id = if n == 1 {
                meta.report_id.clone()
            } else {
                format!("{}.{}", meta.report_id, i + 1)
            };
            let m = ReportMeta {
                report_id: report_id.clone(),
                ..meta.clone()
            };
            let xml = build_aggregate_xml(&m, policy, &chunk);
            let xml_gz = crate::dmarc_report::gzip_xml(xml.as_bytes());
            BuiltReport {
                report_id,
                xml,
                xml_gz,
            }
        })
        .collect()
}

/// Room left under the cap for a split part's `.<n>` id suffix, which the fit
/// test (measuring the unsuffixed id) cannot see: a few literal bytes in the
/// gzip stream, well inside this.
const SPLIT_ID_SLACK_BYTES: u64 = 64;

fn fits(meta: &ReportMeta, policy: &PeerDmarcPolicy, rows: &[ReportRow], cap: u64) -> bool {
    let xml = build_aggregate_xml(meta, policy, rows);
    (crate::dmarc_report::gzip_xml(xml.as_bytes()).len() as u64)
        .saturating_add(SPLIT_ID_SLACK_BYTES)
        <= cap
}

fn collect_chunks(
    meta: &ReportMeta,
    policy: &PeerDmarcPolicy,
    rows: &[ReportRow],
    cap: u64,
) -> Vec<Vec<ReportRow>> {
    if rows.is_empty() {
        return Vec::new();
    }
    if fits(meta, policy, rows, cap) {
        return vec![rows.to_vec()];
    }
    if rows.len() == 1 {
        return Vec::new();
    }
    let mid = split_point(rows);
    let mut v = collect_chunks(meta, policy, &rows[..mid], cap);
    v.extend(collect_chunks(meta, policy, &rows[mid..], cap));
    v
}

/// Where to halve `rows` (len ≥ 2): the From-domain boundary nearest the
/// middle, else the middle.
fn split_point(rows: &[ReportRow]) -> usize {
    let mid = rows.len() / 2;
    let boundaries: Vec<usize> = (1..rows.len())
        .filter(|&i| rows[i].header_from != rows[i - 1].header_from)
        .collect();
    boundaries
        .into_iter()
        .min_by_key(|&i| i.abs_diff(mid))
        .unwrap_or(mid)
}

/// The RFC 7489 §7.2.1.1 attachment name:
/// `<receiver>!<policy-domain>!<begin>!<end>!<report-id>.xml.gz`.
pub fn report_filename(meta: &ReportMeta, policy_domain: &str) -> String {
    let unique: String = meta
        .report_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!(
        "{}!{}!{}!{}!{}.xml.gz",
        meta.org_name, policy_domain, meta.begin, meta.end, unique
    )
}

fn push_line(out: &mut Vec<u8>, line: &str) {
    out.extend_from_slice(line.as_bytes());
    out.extend_from_slice(b"\r\n");
}

/// Frame one report as the mail RFC 7489 §7.2.1.1 describes: `From:` our
/// `dmarc-report@` address, `Subject: Report Domain: <policy domain>
/// Submitter: <our domain> Report-ID: <id>`, a short text part and the
/// gzipped XML as an `application/gzip` attachment, base64 so the message is
/// 7-bit clean. `rfc2822_date` is the `Date:` value (the caller's clock).
pub fn build_report_mime(
    rcpt: &str,
    meta: &ReportMeta,
    policy_domain: &str,
    rfc2822_date: &str,
    xml_gz: &[u8],
) -> Vec<u8> {
    let boundary = format!("dmarc-report-{}", meta.report_id.replace(['@', ' '], "."));
    let mut out = Vec::with_capacity(xml_gz.len() * 2 + 1024);
    push_line(&mut out, &format!("From: {}", meta.email));
    push_line(&mut out, &format!("To: {rcpt}"));
    push_line(
        &mut out,
        &format!(
            "Subject: Report Domain: {policy_domain} Submitter: {} Report-ID: <{}>",
            meta.org_name, meta.report_id
        ),
    );
    push_line(
        &mut out,
        &format!(
            "Message-ID: <{}@{}>",
            meta.report_id.replace('@', "."),
            meta.org_name
        ),
    );
    push_line(&mut out, &format!("Date: {rfc2822_date}"));
    push_line(&mut out, "MIME-Version: 1.0");
    push_line(&mut out, "Auto-Submitted: auto-generated");
    push_line(
        &mut out,
        &format!(r#"Content-Type: multipart/mixed; boundary="{boundary}""#),
    );
    push_line(&mut out, "");
    push_line(&mut out, &format!("--{boundary}"));
    push_line(&mut out, "Content-Type: text/plain; charset=utf-8");
    push_line(&mut out, "");
    push_line(
        &mut out,
        &format!(
            "This is a DMARC aggregate report from {} about mail claiming to be from {} (RFC 7489).",
            meta.org_name, policy_domain
        ),
    );
    push_line(&mut out, "");
    push_line(&mut out, &format!("--{boundary}"));
    let filename = report_filename(meta, policy_domain);
    push_line(
        &mut out,
        &format!(r#"Content-Type: application/gzip; name="{filename}""#),
    );
    push_line(
        &mut out,
        &format!(r#"Content-Disposition: attachment; filename="{filename}""#),
    );
    push_line(&mut out, "Content-Transfer-Encoding: base64");
    push_line(&mut out, "");
    out.extend_from_slice(fauna_core::mime_wrap::base64_wrap_76(xml_gz).as_bytes());
    push_line(&mut out, "");
    push_line(&mut out, &format!("--{boundary}--"));
    out
}

#[cfg(test)]
mod tests;
