//! MTA-STS policy fetcher + parser + in-memory cache.
//!
//! Implements RFC 8461 §3.2 (policy fields) and the slice of §3.4 needed for
//! TLSRPT attribution (`policy_type="sts"` per RFC 8460 §4.3). The pure-Rust
//! parser, mode enum, and serialization-stable `policy_strings()` projection
//! live here, alongside the live DNS+HTTPS fetcher (`LiveMtaStsFetcher`, gated
//! by the `outbound-net` feature) behind the `MtaStsFetcher` trait so tests can
//! mock without touching the network.
//!
//! Goal doc: `docs/goal/behavior/smtp-server.md` § MX resolution (item 3,
//! "MTA-STS policy fetch per recipient domain") + § TLSRPT outbound reporter.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use thiserror::Error;
use tokio::sync::Notify;

/// MTA-STS `mode:` per RFC 8461 §3.2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MtaStsMode {
    Enforce,
    Testing,
    None,
}

impl MtaStsMode {
    /// The RFC 8461 §3.2 `mode:` token — the inverse of the [`std::str::FromStr`]
    /// impl below, and the **only** place this vocabulary is written.
    ///
    /// Public because it crosses two boundaries besides the policy body it was
    /// written for: `MtaStsPolicyWire::mode` on the `fetch_mta_sts_policy` reply
    /// nest sends the Go bridge, and the same field echoed back on
    /// `report_tls_attempt`. It was private until 2026-08-23, and nest had two
    /// hand-written copies of this three-arm table (one per direction) under a
    /// comment saying so; `libs/fauna-mail/tests/go_wire_outcome_contract.rs`
    /// now pins the tokens against the Go mirror.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Enforce => "enforce",
            Self::Testing => "testing",
            Self::None => "none",
        }
    }

    /// Couple the **published** MTA-STS mode to MX cert reality — the
    /// cert-honesty rule (`docs/goal/architecture/nest/tls-certificates.md`
    /// § D; `docs/goal/behavior/mail-multidomain.md` § Per-domain MTA-STS).
    ///
    /// A domain whose `mail.<domain>` MX is currently serving the always-live
    /// self-signed **floor** (no trusted cert obtainable, or a renewal lapsed —
    /// `tls-certificates.md` § A) must **not** advertise `enforce`: an
    /// MTA-STS-enforcing sender (RFC 8461 §5) that fetched the `enforce` policy
    /// then refuses the non-WebPKI MX, **bouncing inbound mail**. So while the
    /// MX is untrusted, `enforce` downgrades to `testing` — the sender still
    /// attempts STARTTLS and reports failures via TLSRPT but never refuses
    /// delivery — restoring `enforce` once a trusted cert is live. The chosen
    /// mode is a **ceiling**: cert reality can only lower it, so `testing`/`none`
    /// pass through unchanged (never upgraded). `mx_cert_trusted` is the caller's
    /// "the served MX cert is a WebPKI-trusted covering cert" verdict (on the
    /// nest: `cert_health_state != OnFloorRenewNeeded`).
    pub fn coupled_to_cert(&self, mx_cert_trusted: bool) -> MtaStsMode {
        match self {
            MtaStsMode::Enforce if !mx_cert_trusted => MtaStsMode::Testing,
            other => other.clone(),
        }
    }

    /// Read a **stored** `mail_domains.mta_sts_mode` value — our own side, which
    /// has exactly two modes (`mail-multidomain.md` § Wizard steps, step 4: every
    /// domain is stored `testing` and the nest advances it to `enforce`; there is
    /// no `none`). Anything that is not `enforce` reads as `testing` — never
    /// advertise a trust we can't keep, never silently stop publishing a policy.
    /// [`MtaStsMode::None`] stays in the enum for the *other* direction: a peer's
    /// policy, parsed by [`std::str::FromStr`] on the outbound path.
    pub fn from_stored(s: &str) -> MtaStsMode {
        match s {
            "enforce" => MtaStsMode::Enforce,
            _ => MtaStsMode::Testing,
        }
    }
}

/// Parse a bare RFC 8461 §3.2 `mode` token (`enforce` / `testing` / `none`) —
/// the inverse of [`MtaStsMode::as_str`], and the `mode:` line of a full policy
/// body ([`parse_policy`] delegates here): a **peer's** policy, where `none` is
/// a real answer. Unknown tokens are `MtaStsParseError::InvalidMode`. Our own
/// stored mode is read with [`MtaStsMode::from_stored`], not this.
impl std::str::FromStr for MtaStsMode {
    type Err = MtaStsParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "enforce" => Ok(Self::Enforce),
            "testing" => Ok(Self::Testing),
            "none" => Ok(Self::None),
            _ => Err(MtaStsParseError::InvalidMode),
        }
    }
}

/// Parsed MTA-STS policy body (RFC 8461 §3.2). Only the four canonical
/// fields are surfaced; unknown keys are dropped on parse per RFC 8461
/// §3.2 ("STS MTAs MUST ignore any unknown keys").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MtaStsPolicy {
    pub version: String,
    pub mode: MtaStsMode,
    pub mx: Vec<String>,
    pub max_age_secs: u32,
}

/// Parse errors. Each variant covers a single load-bearing failure mode —
/// see the `parse_rejects_missing_*` tests.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum MtaStsParseError {
    #[error("missing or unsupported version (expected STSv1)")]
    UnsupportedVersion,
    #[error("missing or invalid mode (expected enforce|testing|none)")]
    InvalidMode,
    #[error("missing or invalid max_age (expected 1..=31557600)")]
    InvalidMaxAge,
    #[error("at least one mx: pattern is required")]
    MissingMx,
}

fn normalize_dns_name(s: &str) -> String {
    fauna_core::web::normalize_dns_name(s)
}

/// Parse an RFC 8461 §3.2 policy body. Accepts LF or CRLF line endings.
/// Blank lines and `#`-comments are skipped. `mx:` may repeat (order is
/// preserved for [`MtaStsPolicy::policy_strings`] determinism). Unknown
/// keys are ignored.
pub fn parse_policy(body: &str) -> Result<MtaStsPolicy, MtaStsParseError> {
    let mut version: Option<String> = None;
    let mut mode: Option<MtaStsMode> = None;
    let mut mx: Vec<String> = Vec::new();
    let mut max_age_secs: Option<u32> = None;

    for raw in body.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "version" => version = Some(value.to_string()),
            "mode" => {
                mode = Some(value.parse::<MtaStsMode>()?);
            }
            "mx" if !value.is_empty() => {
                mx.push(value.to_string());
            }
            "max_age" => {
                max_age_secs = match value.parse::<u32>() {
                    Ok(n) if (1..=31_557_600).contains(&n) => Some(n),
                    _ => return Err(MtaStsParseError::InvalidMaxAge),
                };
            }
            _ => {
                // RFC 8461 §3.2 — ignore unknown keys.
            }
        }
    }

    let version = version.ok_or(MtaStsParseError::UnsupportedVersion)?;
    if version != "STSv1" {
        return Err(MtaStsParseError::UnsupportedVersion);
    }
    let mode = mode.ok_or(MtaStsParseError::InvalidMode)?;
    let max_age_secs = max_age_secs.ok_or(MtaStsParseError::InvalidMaxAge)?;
    if mx.is_empty() {
        return Err(MtaStsParseError::MissingMx);
    }

    Ok(MtaStsPolicy {
        version,
        mode,
        mx,
        max_age_secs,
    })
}

/// True iff `host` matches at least one of `patterns` per RFC 8461 §4.1:
/// comparison is case-insensitive and trailing-dot tolerant on both sides;
/// non-wildcard patterns require an exact match; `*.<base>` patterns require
/// exactly one prepended label (`*.example.com` matches `mail.example.com`
/// but not `example.com` or `a.b.example.com`).
///
/// Pure, always-available (no `outbound-net` / `multidomain` gate) so the Go
/// mail bridge can reuse it over UniFFI to enforce a policy's `mx:` set
/// against the host it actually connected to. [`MtaStsPolicy::matches_mx`]
/// is the method form bound to a parsed policy's `mx` list.
pub fn mx_patterns_match(patterns: &[String], host: &str) -> bool {
    let host = normalize_dns_name(host);
    patterns.iter().any(|pattern| {
        let pat = normalize_dns_name(pattern);
        if let Some(suffix) = pat.strip_prefix("*.") {
            let Some(label_end) = host.find('.') else {
                return false;
            };
            &host[label_end + 1..] == suffix
        } else {
            host == pat
        }
    })
}

impl MtaStsPolicy {
    /// True iff `mx_host` matches at least one of `self.mx` per RFC 8461
    /// §4.1: comparison is case-insensitive and trailing-dot tolerant on
    /// both sides; non-wildcard patterns require an exact match;
    /// `*.<base>` patterns require exactly one prepended label
    /// (`*.example.com` matches `mail.example.com` but not `example.com`
    /// or `a.b.example.com`). Delegates to the free function
    /// [`mx_patterns_match`].
    pub fn matches_mx(&self, mx_host: &str) -> bool {
        mx_patterns_match(&self.mx, mx_host)
    }

    /// Canonical projection used as the RFC 8460 §4.4 `policy-string`
    /// array. Order: `version → mode → mx (input order) → max_age`. The
    /// round-trip property (parse → policy_strings → join('\n') →
    /// parse_policy yields an equal struct) is the load-bearing
    /// invariant; TLSRPT reports must hash deterministically across
    /// delivery attempts.
    pub fn policy_strings(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(3 + self.mx.len());
        out.push(format!("version: {}", self.version));
        out.push(format!("mode: {}", self.mode.as_str()));
        for pattern in &self.mx {
            out.push(format!("mx: {pattern}"));
        }
        out.push(format!("max_age: {}", self.max_age_secs));
        out
    }
}

/// RFC 8461 §3.2 minimum `max_age` (1 day). A served policy below this is a
/// misconfiguration; [`assemble_policy_body`] debug-asserts against it.
#[cfg(feature = "multidomain")]
pub const MIN_MTA_STS_MAX_AGE_SECS: u32 = 86_400;

/// Assemble the MTA-STS **policy file body** we serve at
/// `https://mta-sts.<domain>/.well-known/mta-sts.txt` per RFC 8461 §3.2 +
/// `docs/goal/behavior/mail-multidomain.md` § Per-domain MTA-STS. This is the
/// publish/serve inverse of [`parse_policy`]; it reuses [`MtaStsPolicy::
/// policy_strings`] so the parse↔assemble round-trip stays exact (the
/// `assemble_round_trips_through_parse` test pins it).
///
/// Lines are LF-separated with a trailing LF per RFC 8461 §3.2. The minimum
/// `max_age` (86400 / 1 day) is a debug-assert on the input rather than a hard
/// error: the served policy is admin-derived from `mail_domains`
/// (`mta_sts_max_age_seconds`, default-floored at the schema), so a sub-minimum
/// value here is a programming error in the caller, not untrusted input.
#[cfg(feature = "multidomain")]
pub fn assemble_policy_body(policy: &MtaStsPolicy) -> String {
    debug_assert!(
        policy.max_age_secs >= MIN_MTA_STS_MAX_AGE_SECS,
        "MTA-STS max_age {} below RFC 8461 §3.2 minimum {MIN_MTA_STS_MAX_AGE_SECS}",
        policy.max_age_secs,
    );
    let mut body = policy.policy_strings().join("\n");
    body.push('\n');
    body
}

/// SHA-256 hex of an assembled policy body, used as the `id=` value in the
/// `_mta-sts.<domain>` TXT record (`v=STSv1; id=<hash>`) so peer MTAs detect
/// policy changes and re-fetch per RFC 8461 §3.1/§3.3. Deterministic for a
/// stable body — [`crate::dns::per_domain::build_mta_sts_txt_record`] calls
/// this to assemble the full TXT record.
#[cfg(feature = "multidomain")]
pub fn compute_policy_version_hash(policy_body: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(policy_body.as_bytes());
    hex::encode(digest)
}

/// One successful policy fetch: the `_mta-sts.<domain>` TXT id paired
/// with the parsed body of `https://mta-sts.<domain>/.well-known/mta-sts.txt`.
/// The id lets the cache notice RFC 8461 §3.3 policy rotations on refetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedPolicy {
    pub id: String,
    pub policy: MtaStsPolicy,
}

/// Outcome of one `_mta-sts.<domain>` lookup. Distinguishing
/// `NotPublished` from `FetchError` / `Invalid` is load-bearing for
/// TLSRPT attribution: per RFC 8461 §5 a published-but-broken STS is
/// treated as no-policy for delivery decisions, but RFC 8460 §4.3 wants
/// the failure mode reported as `sts-policy-fetch-error` /
/// `sts-policy-invalid` (vs. `no-policy-found`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MtaStsLookup {
    /// `_mta-sts.<domain>` TXT not present (or doesn't carry `v=STSv1`
    /// + an `id=`). STS not advertised; TLSRPT attributes
    ///   `policy_type="no-policy-found"`.
    NotPublished,
    /// TXT advertises STS but the HTTPS GET / DNS / network failed.
    /// TLSRPT attributes `policy_type="sts"` with empty `policy_string`
    /// and the `sts-policy-fetch-error` result-type.
    FetchError,
    /// Body fetched but unparseable (RFC 8461 §3.2 violation). TLSRPT
    /// attributes `policy_type="sts"` with empty `policy_string` and
    /// the `sts-policy-invalid` result-type.
    Invalid,
    /// Policy fetched + parsed.
    Found(FetchedPolicy),
}

/// The wire discriminator of an [`MtaStsLookup`] — the `outcome` field of
/// `fauna_protocol::bridge_routing::FetchMtaStsPolicyReply`, and the
/// `mta_sts_outcome` field the Go bridge echoes back on `report_tls_attempt`
/// so nest can rebuild the RFC 8460 §4.4 TLSRPT policy bucket without a second
/// fetch.
///
/// It is a separate type from [`MtaStsLookup`] because the wire carries the
/// *discriminator* and the policy body as two fields: `outcome` alone is
/// meaningful (the bridge switches on it), while only `Found` has a body. Nest
/// therefore needs to name the discriminator without a `FetchedPolicy` in hand.
///
/// **This is the single owner of the four tokens.** Until 2026-08-23 they were
/// hand-written at five production sites across two binaries and two languages
/// — nest's producer, nest's inverse consumer, nest's test hook, and the Go
/// bridge's bare literals in `internal/mta/outbound.go` — with the vocabulary
/// itself living only in prose on the wire struct. RFC 8461 §5 makes that
/// load-bearing in the safety direction: `fetch_error` and `invalid` must be
/// treated as no-policy and must never force plaintext, so a token one side
/// stops recognising is a delivery decision made on a fallback arm.
/// `libs/fauna-mail/tests/go_wire_outcome_contract.rs` pins these tokens
/// against the Go mirror; see its module doc for why the existing
/// `reply-*.cbor` round-trip fixtures structurally cannot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MtaStsOutcome {
    NotPublished,
    FetchError,
    Invalid,
    Found,
}

impl MtaStsOutcome {
    /// The wire token. Exhaustive by construction — never add a `_` arm.
    pub fn as_wire(&self) -> &'static str {
        match self {
            Self::NotPublished => "not_published",
            Self::FetchError => "fetch_error",
            Self::Invalid => "invalid",
            Self::Found => "found",
        }
    }

    /// Parse a wire token. `None` for anything else: a peer that sends an
    /// outcome we do not know is answered with a protocol error rather than a
    /// guess, because every candidate guess is a delivery decision (RFC 8461
    /// §5) and the fail-safe direction is not the same for all four.
    pub fn from_wire(token: &str) -> Option<Self> {
        match token {
            "not_published" => Some(Self::NotPublished),
            "fetch_error" => Some(Self::FetchError),
            "invalid" => Some(Self::Invalid),
            "found" => Some(Self::Found),
            _ => None,
        }
    }

    /// Whether this outcome carries a usable policy body. The other three are
    /// all "no policy applies" for delivery decisions per RFC 8461 §5, even
    /// though RFC 8460 §4.3 reports them as three different TLSRPT
    /// result-types.
    pub fn carries_policy(&self) -> bool {
        matches!(self, Self::Found)
    }
}

impl MtaStsLookup {
    /// This lookup's wire discriminator.
    pub fn outcome(&self) -> MtaStsOutcome {
        match self {
            Self::NotPublished => MtaStsOutcome::NotPublished,
            Self::FetchError => MtaStsOutcome::FetchError,
            Self::Invalid => MtaStsOutcome::Invalid,
            Self::Found(_) => MtaStsOutcome::Found,
        }
    }

    /// The payload-free lookup an outcome names, or `None` for [`MtaStsOutcome::Found`],
    /// which needs a [`FetchedPolicy`] the caller must supply from the
    /// accompanying wire field.
    pub fn from_outcome(outcome: MtaStsOutcome) -> Option<Self> {
        match outcome {
            MtaStsOutcome::NotPublished => Some(Self::NotPublished),
            MtaStsOutcome::FetchError => Some(Self::FetchError),
            MtaStsOutcome::Invalid => Some(Self::Invalid),
            MtaStsOutcome::Found => None,
        }
    }
}

/// Wall-clock seconds-since-epoch source — abstracted so caches and
/// retries can run on a manual clock in tests. Mirrors
/// `fauna_bridge_smtp::queue::ClockFn`.
pub type ClockFn = Arc<dyn Fn() -> i64 + Send + Sync>;

/// MTA-STS policy lookup behind a `dyn`-compatible boundary so the bridge
/// can hold an `Arc<dyn MtaStsFetcher>` and tests can substitute a mock.
/// The trait is one-shot: a single `lookup` call covers both the
/// `_mta-sts.<domain>` TXT lookup (for the policy id) and the
/// `https://mta-sts.<domain>/.well-known/mta-sts.txt` GET (for the body).
/// The 4-variant [`MtaStsLookup`] return surfaces the four outcomes
/// TLSRPT attribution distinguishes (RFC 8460 §4.3): not-published,
/// fetch-error, invalid, and found. A transient fetch failure never
/// forces plaintext fallback at `deliver_to_host`; the caller sees
/// `MtaStsLookup::FetchError` and proceeds with the standard delivery
/// path while recording the TLSRPT signal.
#[async_trait::async_trait]
pub trait MtaStsFetcher: Send + Sync {
    async fn lookup(&self, domain: &str) -> anyhow::Result<MtaStsLookup>;
}

/// Fetcher that always returns [`MtaStsLookup::NotPublished`]. Used by
/// test fixtures so callers that don't model STS at all can still satisfy
/// the `MtaStsFetcher` boundary.
pub struct NullMtaStsFetcher;

#[async_trait::async_trait]
impl MtaStsFetcher for NullMtaStsFetcher {
    async fn lookup(&self, _domain: &str) -> anyhow::Result<MtaStsLookup> {
        Ok(MtaStsLookup::NotPublished)
    }
}

/// Parse `v=STSv1; id=<id>` out of a `_mta-sts.<domain>` TXT record per
/// RFC 8461 §3.1.
#[cfg(feature = "outbound-net")]
fn parse_mta_sts_txt_id(record: &str) -> Option<String> {
    if !record.contains("v=STSv1") {
        return None;
    }
    for tag in record.split(';').map(str::trim) {
        if let Some(id) = tag.strip_prefix("id=") {
            let id = id.trim().to_string();
            if !id.is_empty() {
                return Some(id);
            }
        }
    }
    None
}

/// Live `MtaStsFetcher` over `hickory-resolver` (DNS) + `reqwest` (HTTPS).
/// The production outbound path holds an `Arc<dyn MtaStsFetcher>` (usually
/// wrapped in [`CachingMtaStsFetcher`]); this is the network-touching impl.
///
/// Native-only (`outbound-net` feature): `reqwest` + `hickory-resolver` do
/// not cross-compile to wasm32 and must not enter the client FFI surface.
/// Lifted from the retired `fauna-bridge-smtp::outbound::LiveResolver`
/// so the permanent Go-bridge → nest outbound path doesn't couple to that
/// retired crate.
#[cfg(feature = "outbound-net")]
pub struct LiveMtaStsFetcher {
    inner: hickory_resolver::TokioResolver,
    http: reqwest::Client,
}

#[cfg(feature = "outbound-net")]
impl LiveMtaStsFetcher {
    pub fn new() -> anyhow::Result<Self> {
        use anyhow::Context;
        let inner = crate::outbound::build_hickory_resolver()?;
        // 10 s timeout matches the per-MX-host connect budget in the
        // goal doc § MX resolution. No redirect-following: RFC 8461
        // §3.2 says the policy MUST be served at the canonical URL.
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("failed to build reqwest client for MTA-STS")?;
        Ok(Self { inner, http })
    }
}

#[cfg(feature = "outbound-net")]
#[async_trait::async_trait]
impl MtaStsFetcher for LiveMtaStsFetcher {
    async fn lookup(&self, domain: &str) -> anyhow::Result<MtaStsLookup> {
        let query = format!("_mta-sts.{domain}");
        let resp = match self.inner.txt_lookup(&query).await {
            Ok(r) => r,
            // No `_mta-sts.<domain>` TXT — STS not advertised.
            Err(_) => return Ok(MtaStsLookup::NotPublished),
        };
        let mut id: Option<String> = None;
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
            if let Some(parsed) = parse_mta_sts_txt_id(&joined) {
                id = Some(parsed);
                break;
            }
        }
        let id = match id {
            Some(id) => id,
            None => return Ok(MtaStsLookup::NotPublished),
        };

        let url = format!("https://mta-sts.{domain}/.well-known/mta-sts.txt");
        let response = match self.http.get(&url).send().await {
            Ok(r) if r.status().is_success() => r,
            // TXT advertises STS but the body fetch failed (non-2xx /
            // network) — TLSRPT records `sts-policy-fetch-error`; per
            // RFC 8461 §5 delivery proceeds as if no policy.
            Ok(_) | Err(_) => return Ok(MtaStsLookup::FetchError),
        };
        let body = match response.text().await {
            Ok(b) => b,
            Err(_) => return Ok(MtaStsLookup::FetchError),
        };
        match parse_policy(&body) {
            Ok(policy) => Ok(MtaStsLookup::Found(FetchedPolicy { id, policy })),
            // Body fetched but unparseable — TLSRPT records
            // `sts-policy-invalid`.
            Err(_) => Ok(MtaStsLookup::Invalid),
        }
    }
}

/// Negative-cache TTL — short enough that a recipient publishing STS for
/// the first time gets picked up quickly, long enough to absorb
/// per-delivery cache-lookup pressure. RFC 8461 §3.4 only mandates
/// positive caching (`max_age`); negative caching is a project policy
/// choice.
pub const NEGATIVE_CACHE_TTL_SECS: i64 = 60;

#[derive(Clone)]
struct CacheEntry {
    lookup: MtaStsLookup,
    fetched_at: i64,
}

/// In-memory `max_age`-respecting cache wrapping any `MtaStsFetcher`.
/// Within `max_age_secs` ([`MtaStsLookup::Found`] entries) or
/// [`NEGATIVE_CACHE_TTL_SECS`] (every other variant) the inner fetcher
/// is not called. Refetch detects id rotation per RFC 8461 §3.3.
///
/// Concurrent lookups for the same `<domain>` on a cold cache are
/// deduped via a per-domain in-flight slot: the first caller becomes the
/// leader and runs `inner.lookup`; later callers park on a shared
/// [`Notify`] until the leader populates the cache, then re-check the
/// cache. This keeps a sudden burst of outbound deliveries to the same
/// recipient from fanning out N concurrent HTTPS GETs to
/// `mta-sts.<domain>`.
pub struct CachingMtaStsFetcher<F: MtaStsFetcher> {
    inner: F,
    clock: ClockFn,
    cache: Mutex<HashMap<String, CacheEntry>>,
    in_flight: Mutex<HashMap<String, Arc<Notify>>>,
}

impl<F: MtaStsFetcher> CachingMtaStsFetcher<F> {
    pub fn new(inner: F, clock: ClockFn) -> Self {
        Self {
            inner,
            clock,
            cache: Mutex::new(HashMap::new()),
            in_flight: Mutex::new(HashMap::new()),
        }
    }

    /// Accessor for the wrapped fetcher — tests assert on call counters.
    pub fn inner(&self) -> &F {
        &self.inner
    }

    fn now(&self) -> i64 {
        (self.clock)()
    }

    fn ttl_secs(lookup: &MtaStsLookup) -> i64 {
        match lookup {
            MtaStsLookup::Found(fp) => fp.policy.max_age_secs as i64,
            MtaStsLookup::NotPublished | MtaStsLookup::FetchError | MtaStsLookup::Invalid => {
                NEGATIVE_CACHE_TTL_SECS
            }
        }
    }

    fn cached(&self, domain: &str, now: i64) -> Option<MtaStsLookup> {
        let cache = self.cache.lock().ok()?;
        let entry = cache.get(domain)?;
        if now.saturating_sub(entry.fetched_at) < Self::ttl_secs(&entry.lookup) {
            Some(entry.lookup.clone())
        } else {
            None
        }
    }

    fn insert(&self, domain: &str, lookup: &MtaStsLookup, now: i64) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(
                domain.to_string(),
                CacheEntry {
                    lookup: lookup.clone(),
                    fetched_at: now,
                },
            );
        }
    }
}

/// Outcome of the per-domain in-flight slot decision. Leader runs the
/// inner fetch; follower waits on the shared notify and then re-checks
/// the cache.
enum SingleflightRole {
    Leader(Arc<Notify>),
    Follower(Arc<Notify>),
}

#[async_trait::async_trait]
impl<F: MtaStsFetcher> MtaStsFetcher for CachingMtaStsFetcher<F> {
    async fn lookup(&self, domain: &str) -> anyhow::Result<MtaStsLookup> {
        loop {
            let now = self.now();
            if let Some(hit) = self.cached(domain, now) {
                return Ok(hit);
            }

            let role = {
                let mut guard = self
                    .in_flight
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                match guard.get(domain) {
                    Some(existing) => SingleflightRole::Follower(existing.clone()),
                    None => {
                        let notify = Arc::new(Notify::new());
                        guard.insert(domain.to_string(), notify.clone());
                        SingleflightRole::Leader(notify)
                    }
                }
            };

            match role {
                SingleflightRole::Follower(notify) => {
                    // Park on the leader's notify. `Notified::enable`
                    // registers the future as a waiter synchronously so
                    // a `notify_waiters` call between this point and the
                    // `.await` is not lost.
                    let notified = notify.notified();
                    tokio::pin!(notified);
                    notified.as_mut().enable();
                    notified.await;
                    // Cache will be populated if the leader's fetch
                    // succeeded; if it errored, the loop re-runs and we
                    // become the new leader.
                    continue;
                }
                SingleflightRole::Leader(notify) => {
                    let result = self.inner.lookup(domain).await;
                    if let Ok(ref lookup) = result {
                        self.insert(domain, lookup, self.now());
                    }
                    {
                        let mut guard = self
                            .in_flight
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        guard.remove(domain);
                    }
                    notify.notify_waiters();
                    return result;
                }
            }
        }
    }
}

#[cfg(test)]
mod mx_patterns_match_tests {
    //! Direct tests for the pure free function exported over UniFFI for the
    //! Go mail bridge (RFC 8461 §4.1). The method form
    //! [`MtaStsPolicy::matches_mx`] delegates here, so the
    //! `matches_mx_*` integration tests in
    //! `tests/outbound_mta_sts_tests.rs` exercise the same logic via the
    //! policy struct.
    use super::*;

    fn pats(patterns: &[&str]) -> Vec<String> {
        patterns.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn exact_match() {
        assert!(mx_patterns_match(
            &pats(&["mail.example.com"]),
            "mail.example.com"
        ));
        assert!(!mx_patterns_match(
            &pats(&["mail.example.com"]),
            "other.example.com"
        ));
    }

    #[test]
    fn case_insensitive() {
        assert!(mx_patterns_match(
            &pats(&["Mail.Example.Com"]),
            "MAIL.EXAMPLE.COM"
        ));
    }

    #[test]
    fn trailing_dot_tolerant_on_both_sides() {
        assert!(mx_patterns_match(
            &pats(&["mail.example.com."]),
            "mail.example.com"
        ));
        assert!(mx_patterns_match(
            &pats(&["mail.example.com"]),
            "mail.example.com."
        ));
    }

    #[test]
    fn wildcard_matches_exactly_one_prepended_label() {
        let p = pats(&["*.example.com"]);
        assert!(mx_patterns_match(&p, "mail.example.com"));
        // Apex (zero labels) does not match.
        assert!(!mx_patterns_match(&p, "example.com"));
        // Two labels do not match.
        assert!(!mx_patterns_match(&p, "a.b.example.com"));
    }

    #[test]
    fn any_one_pattern_matches() {
        let p = pats(&["mx1.example.com", "*.alt.example.com"]);
        assert!(mx_patterns_match(&p, "mx1.example.com"));
        assert!(mx_patterns_match(&p, "eu.alt.example.com"));
        assert!(!mx_patterns_match(&p, "mx2.example.com"));
    }

    #[test]
    fn empty_patterns_never_match() {
        assert!(!mx_patterns_match(&[], "mail.example.com"));
    }
}

#[cfg(test)]
mod coupled_to_cert_tests {
    //! The MTA-STS↔cert-honesty coupling rule (`tls-certificates.md` § D).
    use super::*;

    #[test]
    fn enforce_downgrades_to_testing_when_mx_untrusted() {
        // A domain whose MX serves the self-signed floor must NOT advertise
        // enforce — an enforcing sender (RFC 8461 §5) refuses the non-WebPKI MX.
        assert_eq!(
            MtaStsMode::Enforce.coupled_to_cert(false),
            MtaStsMode::Testing
        );
    }

    #[test]
    fn enforce_stays_when_mx_trusted() {
        assert_eq!(
            MtaStsMode::Enforce.coupled_to_cert(true),
            MtaStsMode::Enforce
        );
    }

    #[test]
    fn testing_and_none_are_never_upgraded() {
        // The chosen mode is a ceiling; cert reality can only lower it.
        assert_eq!(
            MtaStsMode::Testing.coupled_to_cert(false),
            MtaStsMode::Testing
        );
        assert_eq!(
            MtaStsMode::Testing.coupled_to_cert(true),
            MtaStsMode::Testing
        );
        assert_eq!(MtaStsMode::None.coupled_to_cert(false), MtaStsMode::None);
        assert_eq!(MtaStsMode::None.coupled_to_cert(true), MtaStsMode::None);
    }

    #[test]
    fn from_stored_knows_two_modes_only() {
        // Our own stored mode: `enforce` or `testing`. `none` is a peer's answer,
        // never ours, so a stored `none` (or garbage) reads as `testing`.
        assert_eq!(MtaStsMode::from_stored("enforce"), MtaStsMode::Enforce);
        assert_eq!(MtaStsMode::from_stored("testing"), MtaStsMode::Testing);
        assert_eq!(MtaStsMode::from_stored("none"), MtaStsMode::Testing);
        assert_eq!(MtaStsMode::from_stored("garbage"), MtaStsMode::Testing);
    }

    #[test]
    fn from_str_round_trips_the_peer_policy_tokens() {
        // A peer's policy `mode:` line parses through `FromStr`; the three tokens
        // round-trip via `as_str`.
        for m in [MtaStsMode::Enforce, MtaStsMode::Testing, MtaStsMode::None] {
            assert_eq!(m.as_str().parse::<MtaStsMode>().unwrap(), m);
        }
        assert_eq!(
            "enforce".parse::<MtaStsMode>().unwrap(),
            MtaStsMode::Enforce
        );
        assert_eq!(
            "garbage".parse::<MtaStsMode>(),
            Err(MtaStsParseError::InvalidMode)
        );
    }
}

#[cfg(all(test, feature = "outbound-net"))]
mod live_fetcher_tests {
    //! Tests for the `_mta-sts.<domain>` TXT id parser lifted alongside
    //! [`LiveMtaStsFetcher`] (RFC 8461 §3.1: `v=STSv1; id=<id>`).
    use super::*;

    #[test]
    fn parses_id_from_well_formed_txt() {
        assert_eq!(
            parse_mta_sts_txt_id("v=STSv1; id=20210803T010101"),
            Some("20210803T010101".to_string())
        );
    }

    #[test]
    fn tolerates_surrounding_whitespace_on_each_tag() {
        // Each `;`-split tag is trimmed, and the `id=` value is trimmed,
        // but there is no space allowed *around* the `=` inside the tag
        // (the parser uses `strip_prefix("id=")`, matching RFC 8461 §3.1
        // `name=value` token syntax — no whitespace around `=`).
        assert_eq!(
            parse_mta_sts_txt_id("  v=STSv1 ; id=abc123 "),
            Some("abc123".to_string())
        );
    }

    #[test]
    fn rejects_missing_version() {
        assert_eq!(parse_mta_sts_txt_id("id=20210803T010101"), None);
    }

    #[test]
    fn rejects_missing_id() {
        assert_eq!(parse_mta_sts_txt_id("v=STSv1;"), None);
    }

    #[test]
    fn rejects_empty_id() {
        assert_eq!(parse_mta_sts_txt_id("v=STSv1; id="), None);
    }
}

#[cfg(all(test, feature = "multidomain"))]
mod multidomain_tests {
    use super::*;

    fn enforce_policy() -> MtaStsPolicy {
        MtaStsPolicy {
            version: "STSv1".to_string(),
            mode: MtaStsMode::Enforce,
            mx: vec!["mail.example.com".to_string()],
            max_age_secs: 604_800,
        }
    }

    #[test]
    fn assemble_emits_rfc_8461_body_with_trailing_lf() {
        let body = assemble_policy_body(&enforce_policy());
        assert_eq!(
            body,
            "version: STSv1\nmode: enforce\nmx: mail.example.com\nmax_age: 604800\n"
        );
    }

    #[test]
    fn assemble_emits_one_mx_line_per_host() {
        let mut policy = enforce_policy();
        policy.mx = vec![
            "mail.example.com".to_string(),
            "mx2.example.com".to_string(),
        ];
        let body = assemble_policy_body(&policy);
        assert_eq!(
            body,
            "version: STSv1\nmode: enforce\nmx: mail.example.com\nmx: mx2.example.com\nmax_age: 604800\n"
        );
    }

    #[test]
    fn assemble_round_trips_through_parse() {
        let policy = enforce_policy();
        let body = assemble_policy_body(&policy);
        let reparsed = parse_policy(&body).expect("assembled body must reparse");
        assert_eq!(reparsed, policy);
    }

    #[test]
    fn version_hash_is_deterministic() {
        let body = assemble_policy_body(&enforce_policy());
        assert_eq!(
            compute_policy_version_hash(&body),
            compute_policy_version_hash(&body)
        );
    }

    #[test]
    fn version_hash_changes_with_body() {
        let mut testing = enforce_policy();
        testing.mode = MtaStsMode::Testing;
        let h_enforce = compute_policy_version_hash(&assemble_policy_body(&enforce_policy()));
        let h_testing = compute_policy_version_hash(&assemble_policy_body(&testing));
        assert_ne!(h_enforce, h_testing);
        // SHA-256 hex is 64 chars.
        assert_eq!(h_enforce.len(), 64);
    }

    #[test]
    fn version_hash_matches_known_sha256() {
        // SHA-256 of the empty string, as a cross-check that we hash the raw
        // body bytes with no salt/prefix.
        assert_eq!(
            compute_policy_version_hash(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "below RFC 8461")]
    fn assemble_debug_asserts_minimum_max_age() {
        let mut policy = enforce_policy();
        policy.max_age_secs = MIN_MTA_STS_MAX_AGE_SECS - 1;
        let _ = assemble_policy_body(&policy);
    }
}
