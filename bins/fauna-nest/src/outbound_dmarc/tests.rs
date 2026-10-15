use super::*;
use crate::db::CacheDb;
use fauna_core::mail_auth::{DkimAuthResult, DmarcPolicy};
use std::collections::HashMap;

const NOW: i64 = 1_779_840_000; // 2026-05-27 00:00:00 UTC

struct MapFetcher(HashMap<String, Vec<String>>);

impl MapFetcher {
    fn new(entries: &[(&str, &str)]) -> Self {
        let mut m: HashMap<String, Vec<String>> = HashMap::new();
        for (k, v) in entries {
            m.entry(k.to_string()).or_default().push(v.to_string());
        }
        Self(m)
    }
}

#[async_trait::async_trait]
impl DmarcRecordFetcher for MapFetcher {
    async fn txt(&self, name: &str) -> anyhow::Result<Vec<String>> {
        Ok(self.0.get(name).cloned().unwrap_or_default())
    }
}

fn obs(from: &str) -> DmarcObservation {
    DmarcObservation {
        header_from_domain: from.into(),
        envelope_from_domain: from.into(),
        dkim_aligned: true,
        spf_aligned: false,
        dkim_results: vec![DkimAuthResult {
            domain: from.into(),
            selector: "s1".into(),
            result: "pass".into(),
        }],
        spf_result: "softfail".into(),
    }
}

async fn state() -> Arc<AppState> {
    let state = Arc::new(AppState::for_test(Arc::new(
        CacheDb::open_in_memory().unwrap(),
    )));
    state
        .db
        .add_mail_domain("mta.test", true, "testing", "expand_primary", None, None)
        .await
        .unwrap();
    state
}

fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}

#[tokio::test]
async fn our_own_domains_are_not_counted() {
    let state = state().await;
    assert!(
        !record_observation(
            &state,
            ip("192.0.2.1"),
            &DmarcVerdict::Pass,
            &obs("mta.test")
        )
        .await
        .unwrap()
    );
    assert!(
        !record_observation(
            &state,
            ip("192.0.2.1"),
            &DmarcVerdict::Pass,
            &obs("sub.mta.test")
        )
        .await
        .unwrap()
    );
    assert!(
        record_observation(
            &state,
            ip("192.0.2.1"),
            &DmarcVerdict::Pass,
            &obs("peer.test")
        )
        .await
        .unwrap()
    );
    assert_eq!(
        state
            .email
            .dmarc_aggregator
            .lock()
            .unwrap()
            .recorded_domains(),
        vec!["peer.test".to_string()]
    );
}

#[tokio::test]
async fn a_refused_message_is_counted_as_rejected() {
    let state = state().await;
    let reject = DmarcVerdict::Fail {
        policy: DmarcPolicy::Reject,
    };
    record_observation(&state, ip("192.0.2.1"), &reject, &obs("peer.test"))
        .await
        .unwrap();
    let rows = state
        .email
        .dmarc_aggregator
        .lock()
        .unwrap()
        .rows("peer.test");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        fauna_mail::dmarc_report::Disposition::from(rows[0].disposition),
        fauna_mail::dmarc_report::Disposition::Reject,
        "the default enforcement switches refuse a p=reject failure"
    );
}

#[tokio::test]
async fn the_daily_pass_queues_a_report_to_the_published_mailbox() {
    let state = state().await;
    for _ in 0..3 {
        record_observation(
            &state,
            ip("192.0.2.1"),
            &DmarcVerdict::Pass,
            &obs("mail.peer.test"),
        )
        .await
        .unwrap();
    }
    let fetcher = MapFetcher::new(&[(
        "_dmarc.peer.test",
        "v=DMARC1; p=quarantine; rua=https://r.peer.test/x,mailto:agg@peer.test",
    )]);
    assert_eq!(run_one_emit_pass(&state, &fetcher, NOW).await, 1);

    let due = state.db.fetch_due_outbound(NOW, 10).await.unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].original_sender, "dmarc-report@mta.test");
    assert_eq!(due[0].recipient, "agg@peer.test");

    let sent = state
        .db
        .fetch_dmarc_outbound_reports_for_test()
        .await
        .unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].peer_domain, "peer.test");
    assert_eq!(sent[0].destination, "agg@peer.test");
    assert_eq!((sent[0].begin_at, sent[0].end_at), (NOW - 86_400, NOW - 1));
    let xml = fauna_mail::dmarc_report::gunzip_xml(&sent[0].payload_xml_gz).unwrap();
    let report = fauna_mail::dmarc_report::parse_aggregate_xml(&xml).unwrap();
    assert_eq!(report.policy_domain, "peer.test");
    assert_eq!(report.email, "dmarc-report@mta.test");
    assert_eq!(report.summary().message_count, 3);

    assert!(
        state
            .email
            .dmarc_aggregator
            .lock()
            .unwrap()
            .recorded_domains()
            .is_empty(),
        "the window closes with the pass"
    );
}

#[tokio::test]
async fn a_peer_without_a_mailto_rua_gets_nothing() {
    let state = state().await;
    for from in ["norua.test", "https.test", "nopolicy.test"] {
        record_observation(&state, ip("192.0.2.1"), &DmarcVerdict::Pass, &obs(from))
            .await
            .unwrap();
    }
    let fetcher = MapFetcher::new(&[
        ("_dmarc.norua.test", "v=DMARC1; p=reject"),
        (
            "_dmarc.https.test",
            "v=DMARC1; p=reject; rua=https://r.https.test/x",
        ),
    ]);
    assert_eq!(run_one_emit_pass(&state, &fetcher, NOW).await, 0);
    assert!(
        state
            .db
            .fetch_due_outbound(NOW, 10)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        state
            .email
            .dmarc_aggregator
            .lock()
            .unwrap()
            .recorded_domains()
            .is_empty()
    );
}

#[tokio::test]
async fn a_foreign_mailbox_gets_the_report_only_when_it_invited_it() {
    let state = state().await;
    for from in ["invited.test", "uninvited.test"] {
        record_observation(&state, ip("192.0.2.1"), &DmarcVerdict::Pass, &obs(from))
            .await
            .unwrap();
    }
    let fetcher = MapFetcher::new(&[
        (
            "_dmarc.invited.test",
            "v=DMARC1; p=none; rua=mailto:dmarc@reports.example",
        ),
        (
            "_dmarc.uninvited.test",
            "v=DMARC1; p=none; rua=mailto:victim@victim.example",
        ),
        ("invited.test._report._dmarc.reports.example", "v=DMARC1"),
    ]);
    assert_eq!(run_one_emit_pass(&state, &fetcher, NOW).await, 1);
    let due = state.db.fetch_due_outbound(NOW, 10).await.unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].recipient, "dmarc@reports.example");
}

#[tokio::test]
async fn sent_reports_are_kept_seven_days() {
    let state = state().await;
    let now = crate::db::now_epoch_secs();
    for (id, sent_at) in [("old", now - 8 * 86_400), ("new", now - 86_400)] {
        state
            .db
            .insert_dmarc_outbound_report(&DmarcOutboundReportRow {
                report_id: id.into(),
                peer_domain: "peer.test".into(),
                destination: "agg@peer.test".into(),
                begin_at: 0,
                end_at: 1,
                sent_at,
                payload_xml_gz: vec![1],
            })
            .await
            .unwrap();
    }
    prune_sent_reports(&state).await;
    let left = state
        .db
        .fetch_dmarc_outbound_reports_for_test()
        .await
        .unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].report_id, "new");
}
