use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

fn obs(from: &str, dkim: bool, spf: bool) -> DmarcObservation {
    DmarcObservation {
        header_from_domain: from.to_string(),
        envelope_from_domain: from.to_string(),
        dkim_aligned: dkim,
        spf_aligned: spf,
        dkim_results: vec![DkimAuthResult {
            domain: from.to_string(),
            selector: "s1".into(),
            result: if dkim { "pass" } else { "fail" }.into(),
        }],
        spf_result: if spf { "pass" } else { "fail" }.into(),
    }
}

fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}

fn policy(domain: &str) -> PeerDmarcPolicy {
    PeerDmarcPolicy {
        domain: domain.into(),
        record: parse_dmarc_record("v=DMARC1; p=reject; rua=mailto:agg@peer.test").unwrap(),
    }
}

fn meta() -> ReportMeta {
    ReportMeta {
        org_name: "ours.test".into(),
        email: "dmarc-report@ours.test".into(),
        report_id: "20260527.1779840000.0@ours.test".into(),
        begin: 1_779_753_600,
        end: 1_779_839_999,
    }
}

#[test]
fn disposition_follows_the_enforcement_switches() {
    let reject = DmarcVerdict::Fail {
        policy: DmarcPolicy::Reject,
    };
    let quarantine = DmarcVerdict::Fail {
        policy: DmarcPolicy::Quarantine,
    };
    assert_eq!(
        applied_disposition(&reject, true, true, false),
        Disposition::Reject
    );
    // Log-only switches the reject gate off, not quarantine.
    assert_eq!(
        applied_disposition(&reject, true, true, true),
        Disposition::None
    );
    assert_eq!(
        applied_disposition(&reject, false, true, false),
        Disposition::None
    );
    assert_eq!(
        applied_disposition(&quarantine, true, true, true),
        Disposition::Quarantine
    );
    assert_eq!(
        applied_disposition(&quarantine, true, false, false),
        Disposition::None
    );
    assert_eq!(
        applied_disposition(&DmarcVerdict::Pass, true, true, false),
        Disposition::None
    );
}

#[test]
fn identical_messages_count_into_one_row() {
    let mut agg = DmarcAggregator::default();
    for _ in 0..3 {
        assert!(agg.record(
            ip("192.0.2.1"),
            &obs("Peer.Test", true, false),
            Disposition::None
        ));
    }
    assert!(agg.record(
        ip("192.0.2.2"),
        &obs("peer.test", true, false),
        Disposition::None
    ));
    assert!(!agg.record(ip("192.0.2.3"), &obs("", true, true), Disposition::None));
    assert_eq!(agg.recorded_domains(), vec!["peer.test".to_string()]);
    let rows = agg.rows("peer.test");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].source_ip, "192.0.2.1");
    assert_eq!(rows[0].count, 3);
    assert_eq!(rows[1].count, 1);
    agg.clear_domain("peer.test");
    assert!(agg.recorded_domains().is_empty());
}

#[test]
fn a_new_domain_past_the_cap_is_not_counted() {
    let mut agg = DmarcAggregator::default();
    for i in 0..MAX_REPORTED_DOMAINS {
        assert!(agg.record(
            ip("192.0.2.1"),
            &obs(&format!("d{i}.test"), true, true),
            Disposition::None
        ));
    }
    assert!(!agg.record(
        ip("192.0.2.1"),
        &obs("late.test", true, true),
        Disposition::None
    ));
    // A domain already tracked still counts.
    assert!(agg.record(
        ip("192.0.2.1"),
        &obs("d0.test", true, true),
        Disposition::None
    ));
}

#[test]
fn records_are_read_per_rfc_7489() {
    let r = parse_dmarc_record(
        "v=DMARC1; p=Quarantine; sp=reject; adkim=s; pct=50; rua=mailto:a@x.test, https://r.test/x",
    )
    .unwrap();
    assert_eq!(r.p, "quarantine");
    assert_eq!(r.sp.as_deref(), Some("reject"));
    assert_eq!((r.adkim.as_str(), r.aspf.as_str()), ("s", "r"));
    assert_eq!(r.pct, 50);
    assert_eq!(r.rua, vec!["mailto:a@x.test", "https://r.test/x"]);
    assert!(parse_dmarc_record("v=spf1 -all").is_none());
    assert!(
        parse_dmarc_record("p=reject; v=DMARC1").is_none(),
        "v= must be the first tag"
    );
    assert!(parse_dmarc_record("v=DMARC1; p=bogus").is_none());
    assert_eq!(
        parse_dmarc_record("v=DMARC1; rua=mailto:a@x.test")
            .unwrap()
            .p,
        "none",
        "§6.6.3: a missing p= with a rua= reads as none"
    );
    assert!(parse_dmarc_record("v=DMARC1").is_none());
}

#[test]
fn rua_uris_are_mailto_only_with_their_size_limit() {
    assert_eq!(
        parse_rua("mailto:agg@peer.test!10m"),
        RuaDestination::Mailto {
            address: "agg@peer.test".into(),
            max_bytes: Some(10 << 20)
        }
    );
    assert_eq!(
        parse_rua("MAILTO:agg@peer.test?subject=x"),
        RuaDestination::Mailto {
            address: "agg@peer.test".into(),
            max_bytes: None
        }
    );
    assert!(matches!(
        parse_rua("https://peer.test/report"),
        RuaDestination::Unsupported { .. }
    ));
    let (dest, skipped) = plan_destinations(&[
        "https://peer.test/r".into(),
        "mailto:a@peer.test!50k".into(),
        "mailto:never@peer.test".into(),
    ]);
    assert_eq!(dest, vec![("a@peer.test".to_string(), 50 << 10)]);
    assert_eq!(skipped, 1);
    let (dest, _) = plan_destinations(&["mailto:a@peer.test!1g".into()]);
    assert_eq!(
        dest[0].1, DMARC_REPORT_CAP_BYTES,
        "a larger ruri-size never raises our cap"
    );
}

struct MapFetcher {
    map: HashMap<String, Vec<String>>,
    calls: AtomicUsize,
}

impl MapFetcher {
    fn new(entries: &[(&str, &str)]) -> Self {
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        for (k, v) in entries {
            map.entry(k.to_string()).or_default().push(v.to_string());
        }
        Self {
            map,
            calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait::async_trait]
impl DmarcRecordFetcher for MapFetcher {
    async fn txt(&self, name: &str) -> anyhow::Result<Vec<String>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.map.get(name).cloned().unwrap_or_default())
    }
}

#[tokio::test]
async fn policy_discovery_walks_up_to_the_parent_domain() {
    let f = MapFetcher::new(&[(
        "_dmarc.peer.test",
        "v=DMARC1; p=reject; rua=mailto:agg@peer.test",
    )]);
    let p = discover_policy(&f, "Mail.Peer.Test.")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(p.domain, "peer.test");
    assert_eq!(p.record.p, "reject");
    // Never asks a one-label name.
    assert!(discover_policy(&f, "other.test").await.unwrap().is_none());
    assert_eq!(f.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn two_records_at_one_name_are_no_record() {
    let f = MapFetcher::new(&[
        ("_dmarc.peer.test", "v=DMARC1; p=reject"),
        ("_dmarc.peer.test", "v=DMARC1; p=none"),
    ]);
    assert!(discover_policy(&f, "peer.test").await.unwrap().is_none());
}

#[tokio::test]
async fn a_foreign_destination_must_invite_the_report() {
    let f = MapFetcher::new(&[("peer.test._report._dmarc.invited.test", "v=DMARC1")]);
    assert!(external_destination_authorized(&f, "peer.test", "peer.test").await);
    assert!(external_destination_authorized(&f, "peer.test", "reports.peer.test").await);
    assert!(external_destination_authorized(&f, "peer.test", "invited.test").await);
    assert!(!external_destination_authorized(&f, "peer.test", "victim.test").await);
}

#[tokio::test]
async fn the_cache_reads_each_name_once_a_day() {
    let now = Arc::new(std::sync::atomic::AtomicI64::new(1_000));
    let clock_now = now.clone();
    let clock: crate::outbound::mta_sts::ClockFn =
        Arc::new(move || clock_now.load(Ordering::SeqCst));
    let f = CachingDmarcRecordFetcher::new(
        MapFetcher::new(&[("_dmarc.peer.test", "v=DMARC1; p=none")]),
        clock,
    );
    f.txt("_dmarc.peer.test").await.unwrap();
    f.txt("_dmarc.peer.test").await.unwrap();
    assert_eq!(f.inner().calls.load(Ordering::SeqCst), 1);
    now.fetch_add(DMARC_RECORD_CACHE_TTL_SECS, Ordering::SeqCst);
    f.txt("_dmarc.peer.test").await.unwrap();
    assert_eq!(f.inner().calls.load(Ordering::SeqCst), 2);
}

#[test]
fn our_report_reads_back_through_our_own_receiver() {
    let mut agg = DmarcAggregator::default();
    agg.record(
        ip("192.0.2.1"),
        &obs("peer.test", true, false),
        Disposition::None,
    );
    agg.record(
        ip("192.0.2.1"),
        &obs("peer.test", true, false),
        Disposition::None,
    );
    agg.record(
        ip("2001:db8::1"),
        &obs("peer.test", false, false),
        Disposition::Reject,
    );
    let mut o = obs("peer.test", false, false);
    o.envelope_from_domain = "a<b&c.test".into();
    agg.record(ip("192.0.2.9"), &o, Disposition::Quarantine);
    let rows = agg.rows("peer.test");

    let built = build_reports(&meta(), &policy("peer.test"), &rows, DMARC_REPORT_CAP_BYTES);
    assert_eq!(built.len(), 1);
    assert_eq!(built[0].report_id, meta().report_id);
    assert!(built[0].xml.contains("<domain>a&lt;b&amp;c.test</domain>"));

    let mime = build_report_mime(
        "agg@peer.test",
        &meta(),
        "peer.test",
        "Wed, 27 May 2026 00:00:00 +0000",
        &built[0].xml_gz,
    );
    let text = String::from_utf8_lossy(&mime);
    assert!(text.contains("From: dmarc-report@ours.test\r\n"));
    assert!(text.contains(
        "Subject: Report Domain: peer.test Submitter: ours.test Report-ID: <20260527.1779840000.0@ours.test>\r\n"
    ));
    assert!(text.contains(
        "filename=\"ours.test!peer.test!1779753600!1779839999!20260527.1779840000.0_ours.test.xml.gz\""
    ));

    let xml = crate::dmarc_report::extract_report_xml(&mime).unwrap();
    let report = crate::dmarc_report::parse_aggregate_xml(&xml).unwrap();
    assert_eq!(report.org_name, "ours.test");
    assert_eq!(report.email, "dmarc-report@ours.test");
    assert_eq!(report.policy_domain, "peer.test");
    assert_eq!(
        (report.begin_at, report.end_at),
        (1_779_753_600, 1_779_839_999)
    );
    let s = report.summary();
    assert_eq!(s.message_count, 4);
    assert_eq!(s.dkim_pass, 2);
    assert_eq!(s.spf_pass, 0);
    assert_eq!(s.disposition_reject, 1);
    assert_eq!(s.disposition_quarantine, 1);
    assert_eq!(s.disposition_none, 2);
}

#[test]
fn an_oversize_report_splits_without_losing_a_message() {
    let mut agg = DmarcAggregator::default();
    let mut rows = Vec::new();
    for d in 0..6 {
        let from = format!("d{d}.peer.test");
        for i in 0..40u8 {
            agg.record(
                ip(&format!("198.51.100.{i}")),
                &obs(&from, true, true),
                Disposition::None,
            );
        }
        rows.extend(agg.rows(&from));
    }
    let whole = build_reports(&meta(), &policy("peer.test"), &rows, DMARC_REPORT_CAP_BYTES);
    assert_eq!(whole.len(), 1);
    let cap = whole[0].xml_gz.len() as u64 / 3;

    let parts = build_reports(&meta(), &policy("peer.test"), &rows, cap);
    assert!(parts.len() >= 3, "got {} parts", parts.len());
    let mut total = 0;
    for (i, p) in parts.iter().enumerate() {
        assert!(p.xml_gz.len() as u64 <= cap);
        assert_eq!(p.report_id, format!("{}.{}", meta().report_id, i + 1));
        let r = crate::dmarc_report::parse_aggregate_xml(p.xml.as_bytes()).unwrap();
        total += r.summary().message_count;
    }
    assert_eq!(total, 240);
}

#[test]
fn a_row_too_large_for_the_destination_is_dropped() {
    let rows = DmarcAggregator::default().rows("none");
    assert!(build_reports(&meta(), &policy("peer.test"), &rows, 1).is_empty());
    let mut agg = DmarcAggregator::default();
    agg.record(
        ip("192.0.2.1"),
        &obs("peer.test", true, true),
        Disposition::None,
    );
    assert!(build_reports(&meta(), &policy("peer.test"), &agg.rows("peer.test"), 16).is_empty());
}
