//! MTA-STS policy fetcher + parser + cache — docs/goal/behavior/smtp-server.md
//! § MX resolution (item 3) and § TLSRPT outbound reporter (RFC 8460 §4.3
//! `policy_type="sts"` attribution).

#![cfg(feature = "outbound")]

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use fauna_mail::outbound::mta_sts::{
    CachingMtaStsFetcher, FetchedPolicy, MtaStsFetcher, MtaStsLookup, MtaStsMode, MtaStsParseError,
    MtaStsPolicy, NullMtaStsFetcher, parse_policy,
};
use tokio::sync::{Mutex as TokioMutex, Notify};

// ---------------------------------------------------------------------------
// Item 1 (red) — parser extracts the four required fields from the RFC 8461
// §3.2 example body, preserving multiple `mx:` lines in input order.
// ---------------------------------------------------------------------------

#[test]
fn parse_canonical_policy_extracts_fields() {
    let body = "\
version: STSv1
mode: enforce
mx: mail.example.com
mx: *.example.com
max_age: 604800
";
    let policy = parse_policy(body).expect("canonical RFC 8461 §3.2 body should parse");
    assert_eq!(policy.version, "STSv1");
    assert_eq!(policy.mode, MtaStsMode::Enforce);
    assert_eq!(
        policy.mx,
        vec!["mail.example.com".to_string(), "*.example.com".to_string()]
    );
    assert_eq!(policy.max_age_secs, 604_800);
}

// ---------------------------------------------------------------------------
// Item 3 (red) — each MtaStsParseError variant is reachable; blank lines,
// `#`-comments, and CRLF endings are tolerated.
// ---------------------------------------------------------------------------

#[test]
fn parse_rejects_missing_version() {
    let body = "\
mode: enforce
mx: mail.example.com
max_age: 604800
";
    assert_eq!(
        parse_policy(body),
        Err(MtaStsParseError::UnsupportedVersion)
    );
}

#[test]
fn parse_rejects_unsupported_version() {
    let body = "\
version: STSv999
mode: enforce
mx: mail.example.com
max_age: 604800
";
    assert_eq!(
        parse_policy(body),
        Err(MtaStsParseError::UnsupportedVersion)
    );
}

#[test]
fn parse_rejects_missing_mode() {
    let body = "\
version: STSv1
mx: mail.example.com
max_age: 604800
";
    assert_eq!(parse_policy(body), Err(MtaStsParseError::InvalidMode));
}

#[test]
fn parse_rejects_invalid_mode() {
    let body = "\
version: STSv1
mode: bogus
mx: mail.example.com
max_age: 604800
";
    assert_eq!(parse_policy(body), Err(MtaStsParseError::InvalidMode));
}

#[test]
fn parse_rejects_missing_max_age() {
    let body = "\
version: STSv1
mode: enforce
mx: mail.example.com
";
    assert_eq!(parse_policy(body), Err(MtaStsParseError::InvalidMaxAge));
}

#[test]
fn parse_rejects_out_of_range_max_age() {
    let body = "\
version: STSv1
mode: enforce
mx: mail.example.com
max_age: 99999999
";
    assert_eq!(parse_policy(body), Err(MtaStsParseError::InvalidMaxAge));
}

#[test]
fn parse_rejects_missing_mx() {
    let body = "\
version: STSv1
mode: enforce
max_age: 604800
";
    assert_eq!(parse_policy(body), Err(MtaStsParseError::MissingMx));
}

#[test]
fn parse_skips_blank_lines_and_comments() {
    let body = "\
# Issued by example.com on 2026-05-15.

version: STSv1

# Mode is enforce — see RFC 8461 §5 for upgrade plan.
mode: enforce
mx: mail.example.com
   # indented comment
max_age: 604800
";
    let policy = parse_policy(body).expect("blank lines and comments should be skipped");
    assert_eq!(policy.mode, MtaStsMode::Enforce);
    assert_eq!(policy.mx, vec!["mail.example.com".to_string()]);
}

#[test]
fn parse_accepts_crlf_line_endings() {
    let body = "version: STSv1\r\nmode: testing\r\nmx: *.example.com\r\nmax_age: 86400\r\n";
    let policy = parse_policy(body).expect("CRLF line endings should be tolerated");
    assert_eq!(policy.mode, MtaStsMode::Testing);
    assert_eq!(policy.mx, vec!["*.example.com".to_string()]);
    assert_eq!(policy.max_age_secs, 86_400);
}

// ---------------------------------------------------------------------------
// Items 5–6 — `policy_strings()` is the byte-stable RFC 8460 §4.4
// `policy-string` array; round-tripping through `parse_policy` yields an
// equal struct. TLSRPT report determinism depends on this.
// ---------------------------------------------------------------------------

#[test]
fn policy_strings_round_trip() {
    let body = "\
version: STSv1
mode: enforce
mx: mail.example.com
mx: *.example.com
max_age: 604800
";
    let policy = parse_policy(body).unwrap();
    let strings = policy.policy_strings();
    assert_eq!(
        strings,
        vec![
            "version: STSv1".to_string(),
            "mode: enforce".to_string(),
            "mx: mail.example.com".to_string(),
            "mx: *.example.com".to_string(),
            "max_age: 604800".to_string(),
        ],
    );
    let rejoined = strings.join("\n");
    let reparsed = parse_policy(&rejoined).expect("round-trip should re-parse");
    assert_eq!(reparsed, policy);
}

// ---------------------------------------------------------------------------
// Items 9–10 — `MtaStsFetcher` trait + `CachingMtaStsFetcher` wrapper.
// The cache holds per-domain entries with `max_age_secs` TTL for hits and
// `NEGATIVE_CACHE_TTL_SECS` for absences; refetches after TTL detect id
// rotation. Scripted responses + a manual clock keep these tests pure.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct MockMtaStsFetcher {
    /// FIFO of scripted responses. Each `lookup` pops one.
    responses: TokioMutex<std::collections::VecDeque<MtaStsLookup>>,
    /// Per-domain call counter — covers "fetcher is invoked exactly N
    /// times" assertions.
    calls: TokioMutex<Vec<String>>,
    /// Optional release gate. When set, `lookup` awaits `notified()`
    /// before returning the scripted response — lets dedup tests park
    /// the leader call while followers pile up behind it.
    release: Option<Arc<Notify>>,
}

impl MockMtaStsFetcher {
    fn new(scripted: impl IntoIterator<Item = MtaStsLookup>) -> Self {
        Self {
            responses: TokioMutex::new(scripted.into_iter().collect()),
            calls: TokioMutex::new(Vec::new()),
            release: None,
        }
    }

    fn new_gated(scripted: impl IntoIterator<Item = MtaStsLookup>, release: Arc<Notify>) -> Self {
        Self {
            responses: TokioMutex::new(scripted.into_iter().collect()),
            calls: TokioMutex::new(Vec::new()),
            release: Some(release),
        }
    }

    async fn call_count(&self) -> usize {
        self.calls.lock().await.len()
    }
}

#[async_trait::async_trait]
impl MtaStsFetcher for MockMtaStsFetcher {
    async fn lookup(&self, domain: &str) -> anyhow::Result<MtaStsLookup> {
        self.calls.lock().await.push(domain.to_string());
        if let Some(gate) = &self.release {
            gate.notified().await;
        }
        let resp = self.responses.lock().await.pop_front();
        Ok(resp.unwrap_or(MtaStsLookup::NotPublished))
    }
}

fn enforce_policy(mx: &str, max_age_secs: u32) -> MtaStsPolicy {
    MtaStsPolicy {
        version: "STSv1".to_string(),
        mode: MtaStsMode::Enforce,
        mx: vec![mx.to_string()],
        max_age_secs,
    }
}

fn make_clock() -> (Arc<AtomicI64>, Arc<dyn Fn() -> i64 + Send + Sync>) {
    let now = Arc::new(AtomicI64::new(0));
    let now_clone = now.clone();
    let clock: Arc<dyn Fn() -> i64 + Send + Sync> =
        Arc::new(move || now_clone.load(Ordering::Relaxed));
    (now, clock)
}

fn assert_found_eq(actual: MtaStsLookup, expected: &FetchedPolicy) {
    match actual {
        MtaStsLookup::Found(fp) => assert_eq!(&fp, expected),
        other => panic!("expected MtaStsLookup::Found, got {other:?}"),
    }
}

#[tokio::test]
async fn cache_returns_cached_within_max_age() {
    let (clock_state, clock) = make_clock();
    let fp = FetchedPolicy {
        id: "20260515T000000".to_string(),
        policy: enforce_policy("mx.example.com", 600),
    };
    let inner = MockMtaStsFetcher::new([MtaStsLookup::Found(fp.clone())]);
    let cache = CachingMtaStsFetcher::new(inner, clock);

    assert_found_eq(cache.lookup("example.com").await.unwrap(), &fp);

    // 599 s later — still within the 600 s TTL.
    clock_state.store(599, Ordering::Relaxed);
    assert_found_eq(cache.lookup("example.com").await.unwrap(), &fp);
    assert_eq!(
        cache.inner().call_count().await,
        1,
        "cache should hold the response within max_age_secs",
    );
}

#[tokio::test]
async fn cache_refetches_when_max_age_expires() {
    let (clock_state, clock) = make_clock();
    let fp_v1 = FetchedPolicy {
        id: "id-A".to_string(),
        policy: enforce_policy("mx.example.com", 600),
    };
    let fp_v2 = FetchedPolicy {
        id: "id-A".to_string(),
        policy: enforce_policy("mx.example.com", 600),
    };
    let inner = MockMtaStsFetcher::new([MtaStsLookup::Found(fp_v1), MtaStsLookup::Found(fp_v2)]);
    let cache = CachingMtaStsFetcher::new(inner, clock);

    cache.lookup("example.com").await.unwrap();
    // 601 s — TTL exceeded.
    clock_state.store(601, Ordering::Relaxed);
    cache.lookup("example.com").await.unwrap();
    assert_eq!(cache.inner().call_count().await, 2);
}

#[tokio::test]
async fn cache_refetches_when_policy_id_rotates() {
    let (clock_state, clock) = make_clock();
    let fp_a = FetchedPolicy {
        id: "id-A".to_string(),
        policy: enforce_policy("mx1.example.com", 600),
    };
    let fp_b = FetchedPolicy {
        id: "id-B".to_string(),
        policy: enforce_policy("mx2.example.com", 600),
    };
    let inner = MockMtaStsFetcher::new([
        MtaStsLookup::Found(fp_a.clone()),
        MtaStsLookup::Found(fp_b.clone()),
    ]);
    let cache = CachingMtaStsFetcher::new(inner, clock);

    let first = cache.lookup("example.com").await.unwrap();
    match first {
        MtaStsLookup::Found(fp) => assert_eq!(fp.id, "id-A"),
        other => panic!("expected Found(id-A), got {other:?}"),
    }
    // Past TTL — refetch picks up the rotated id and new mx.
    clock_state.store(601, Ordering::Relaxed);
    let second = cache.lookup("example.com").await.unwrap();
    match second {
        MtaStsLookup::Found(fp) => {
            assert_eq!(fp.id, "id-B");
            assert_eq!(fp.policy.mx, vec!["mx2.example.com".to_string()]);
        }
        other => panic!("expected Found(id-B), got {other:?}"),
    }
}

#[tokio::test]
async fn cache_negative_result_within_short_ttl() {
    let (clock_state, clock) = make_clock();
    let inner = MockMtaStsFetcher::new([MtaStsLookup::NotPublished, MtaStsLookup::NotPublished]);
    let cache = CachingMtaStsFetcher::new(inner, clock);

    assert!(matches!(
        cache.lookup("example.com").await.unwrap(),
        MtaStsLookup::NotPublished
    ));
    // Within the 60 s negative-cache TTL — no second call.
    clock_state.store(59, Ordering::Relaxed);
    assert!(matches!(
        cache.lookup("example.com").await.unwrap(),
        MtaStsLookup::NotPublished
    ));
    assert_eq!(cache.inner().call_count().await, 1);

    // After the 60 s TTL — refetch.
    clock_state.store(61, Ordering::Relaxed);
    assert!(matches!(
        cache.lookup("example.com").await.unwrap(),
        MtaStsLookup::NotPublished
    ));
    assert_eq!(cache.inner().call_count().await, 2);
}

#[tokio::test]
async fn null_fetcher_returns_not_published_for_any_domain() {
    let f = NullMtaStsFetcher;
    assert!(matches!(
        f.lookup("example.com").await.unwrap(),
        MtaStsLookup::NotPublished
    ));
    assert!(matches!(
        f.lookup("anotherdomain.org").await.unwrap(),
        MtaStsLookup::NotPublished
    ));
}

// ---------------------------------------------------------------------------
// `MtaStsLookup` distinguishes `FetchError` and
// `Invalid` from `NotPublished` so TLSRPT can attribute `policy_type="sts"`
// with the corresponding `result-type` (`sts-policy-fetch-error` /
// `sts-policy-invalid`). The cache holds these per the negative TTL.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cache_exposes_fetch_error_distinct_from_not_published() {
    let (clock_state, clock) = make_clock();
    let inner = MockMtaStsFetcher::new([MtaStsLookup::FetchError]);
    let cache = CachingMtaStsFetcher::new(inner, clock);

    assert!(matches!(
        cache.lookup("example.com").await.unwrap(),
        MtaStsLookup::FetchError,
    ));
    // Cached within the negative TTL — same variant on the second call,
    // no extra inner invocation.
    clock_state.store(30, Ordering::Relaxed);
    assert!(matches!(
        cache.lookup("example.com").await.unwrap(),
        MtaStsLookup::FetchError,
    ));
    assert_eq!(cache.inner().call_count().await, 1);
}

#[tokio::test]
async fn cache_exposes_invalid_distinct_from_not_published() {
    let (clock_state, clock) = make_clock();
    let inner = MockMtaStsFetcher::new([MtaStsLookup::Invalid]);
    let cache = CachingMtaStsFetcher::new(inner, clock);

    assert!(matches!(
        cache.lookup("example.com").await.unwrap(),
        MtaStsLookup::Invalid,
    ));
    clock_state.store(30, Ordering::Relaxed);
    assert!(matches!(
        cache.lookup("example.com").await.unwrap(),
        MtaStsLookup::Invalid,
    ));
    assert_eq!(cache.inner().call_count().await, 1);
}

// ---------------------------------------------------------------------------
// `MtaStsPolicy::matches_mx` per RFC 8461 §4.1:
// case-insensitive, trailing-dot tolerant, exact match for non-wildcard
// patterns, single-label `*.<base>` wildcards.
// ---------------------------------------------------------------------------

fn enforce_policy_with_patterns(patterns: &[&str]) -> MtaStsPolicy {
    MtaStsPolicy {
        version: "STSv1".to_string(),
        mode: MtaStsMode::Enforce,
        mx: patterns.iter().map(|s| s.to_string()).collect(),
        max_age_secs: 86_400,
    }
}

#[test]
fn matches_mx_exact_match() {
    let p = enforce_policy_with_patterns(&["mail.example.com"]);
    assert!(p.matches_mx("mail.example.com"));
    assert!(!p.matches_mx("other.example.com"));
}

#[test]
fn matches_mx_wildcard_one_label() {
    // RFC 8461 §4.1: `*.example.com` matches one label (`mail.example.com`)
    // but not the apex (`example.com`) and not two labels
    // (`a.b.example.com`).
    let p = enforce_policy_with_patterns(&["*.example.com"]);
    assert!(p.matches_mx("mail.example.com"));
    assert!(!p.matches_mx("example.com"));
    assert!(!p.matches_mx("a.b.example.com"));
}

#[test]
fn matches_mx_case_insensitive() {
    let p = enforce_policy_with_patterns(&["Mail.Example.Com"]);
    assert!(p.matches_mx("MAIL.EXAMPLE.COM"));
    assert!(p.matches_mx("mail.example.com"));
}

#[test]
fn matches_mx_trailing_dot_tolerant() {
    let p = enforce_policy_with_patterns(&["mail.example.com."]);
    assert!(p.matches_mx("mail.example.com"));
    assert!(p.matches_mx("mail.example.com."));
}

#[test]
fn matches_mx_wildcard_does_not_span_two_labels() {
    let p = enforce_policy_with_patterns(&["*.example.com"]);
    assert!(!p.matches_mx("foo.bar.example.com"));
}

#[test]
fn matches_mx_no_match() {
    let p = enforce_policy_with_patterns(&["mx1.example.com", "*.alt.example.com"]);
    assert!(!p.matches_mx("mail.example.com"));
    assert!(!p.matches_mx("example.com"));
    assert!(!p.matches_mx("a.b.alt.example.com"));
}

#[test]
fn matches_mx_against_multiple_patterns() {
    // Any one matching pattern → match.
    let p = enforce_policy_with_patterns(&["mx1.example.com", "*.alt.example.com"]);
    assert!(p.matches_mx("mx1.example.com"));
    assert!(p.matches_mx("eu.alt.example.com"));
    assert!(!p.matches_mx("mx2.example.com"));
}

#[test]
fn policy_strings_lowercases_mode_for_each_variant() {
    for (mode_word, expected_mode) in [
        ("enforce", MtaStsMode::Enforce),
        ("testing", MtaStsMode::Testing),
        ("none", MtaStsMode::None),
    ] {
        let body =
            format!("version: STSv1\nmode: {mode_word}\nmx: mx.example.com\nmax_age: 3600\n");
        let policy = parse_policy(&body).unwrap();
        assert_eq!(policy.mode, expected_mode);
        assert!(
            policy
                .policy_strings()
                .iter()
                .any(|s| s == &format!("mode: {mode_word}")),
            "policy_strings should contain `mode: {mode_word}` for {expected_mode:?}",
        );
    }
}

// ---------------------------------------------------------------------------
// Concurrent-fetch dedup. RFC 8461 §3.2's HTTPS GET is
// expensive (DNS + TCP + TLS + body) and the recipient operator only needs
// one in-flight request per `<domain>` to satisfy multiple concurrent
// outbound deliveries to that domain. Without dedup, a sudden burst to a
// new recipient fans out N concurrent GETs on a cold cache; with dedup the
// followers wait for the leader's result and return it.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cold_cache_dedups_concurrent_lookups_for_same_domain() {
    let (_clock_state, clock) = make_clock();
    let fp = FetchedPolicy {
        id: "id-A".to_string(),
        policy: enforce_policy("mx.example.com", 600),
    };
    let release = Arc::new(Notify::new());
    // Only one scripted response — if the fetcher gets called twice the
    // second call falls back to NotPublished and the assertion fails.
    let inner = MockMtaStsFetcher::new_gated([MtaStsLookup::Found(fp.clone())], release.clone());
    let cache = Arc::new(CachingMtaStsFetcher::new(inner, clock));

    let mut handles = Vec::new();
    for _ in 0..5 {
        let cache = cache.clone();
        handles.push(tokio::spawn(
            async move { cache.lookup("example.com").await },
        ));
    }

    // Yield enough times for every spawned task to reach the gate (the
    // leader hits `release.notified().await` inside `inner.lookup`; the
    // followers park inside the dedup primitive). Two yields per task is
    // sufficient under the current-thread runtime tokio::test uses.
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }

    release.notify_waiters();

    for h in handles {
        let result = h.await.expect("task should not panic").expect("lookup ok");
        assert_found_eq(result, &fp);
    }
    assert_eq!(
        cache.inner().call_count().await,
        1,
        "all 5 concurrent lookups should share one inner.lookup invocation",
    );
}

#[tokio::test]
async fn dedup_does_not_collapse_lookups_for_different_domains() {
    let (_clock_state, clock) = make_clock();
    let fp_a = FetchedPolicy {
        id: "id-A".to_string(),
        policy: enforce_policy("mx.a.example.com", 600),
    };
    let fp_b = FetchedPolicy {
        id: "id-B".to_string(),
        policy: enforce_policy("mx.b.example.com", 600),
    };
    let release = Arc::new(Notify::new());
    let inner = MockMtaStsFetcher::new_gated(
        [
            MtaStsLookup::Found(fp_a.clone()),
            MtaStsLookup::Found(fp_b.clone()),
        ],
        release.clone(),
    );
    let cache = Arc::new(CachingMtaStsFetcher::new(inner, clock));

    let cache_a = cache.clone();
    let h_a = tokio::spawn(async move { cache_a.lookup("a.example.com").await });
    let cache_b = cache.clone();
    let h_b = tokio::spawn(async move { cache_b.lookup("b.example.com").await });

    for _ in 0..16 {
        tokio::task::yield_now().await;
    }

    // Release both gated leader calls (notify_waiters wakes all parked
    // notifications, and the gate is shared across both inner.lookup
    // invocations).
    release.notify_waiters();

    let r_a = h_a.await.unwrap().unwrap();
    let r_b = h_b.await.unwrap().unwrap();
    // Inner pops responses FIFO; the FIFO order matches whichever spawn
    // reached the mock first. Assert both fetched policies appear, in
    // either order.
    let got: std::collections::HashSet<String> = [r_a, r_b]
        .into_iter()
        .map(|lookup| match lookup {
            MtaStsLookup::Found(fp) => fp.id,
            other => panic!("expected Found, got {other:?}"),
        })
        .collect();
    assert_eq!(
        got,
        ["id-A".to_string(), "id-B".to_string()]
            .into_iter()
            .collect(),
    );
    assert_eq!(
        cache.inner().call_count().await,
        2,
        "dedup must be per-domain — different domains get independent fetches",
    );
}

#[tokio::test]
async fn followers_after_leader_completes_hit_the_cache() {
    // Once the leader's lookup populates the cache, subsequent calls
    // (whether they were waiting on the in-flight slot or arrived after
    // it cleared) must observe a cache hit and not start a fresh fetch.
    let (_clock_state, clock) = make_clock();
    let fp = FetchedPolicy {
        id: "id-A".to_string(),
        policy: enforce_policy("mx.example.com", 600),
    };
    let release = Arc::new(Notify::new());
    let inner = MockMtaStsFetcher::new_gated([MtaStsLookup::Found(fp.clone())], release.clone());
    let cache = Arc::new(CachingMtaStsFetcher::new(inner, clock));

    // First lookup is the leader; park it.
    let cache_leader = cache.clone();
    let leader = tokio::spawn(async move { cache_leader.lookup("example.com").await });
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    // Release and let the leader finish.
    release.notify_waiters();
    assert_found_eq(leader.await.unwrap().unwrap(), &fp);

    // A subsequent lookup must come from the cache — no new inner call.
    assert_found_eq(cache.lookup("example.com").await.unwrap(), &fp);
    assert_eq!(cache.inner().call_count().await, 1);
}
