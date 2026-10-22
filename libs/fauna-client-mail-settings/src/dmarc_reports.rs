//! Shared orchestration for the admin `admin-mail-reports` page — the DMARC
//! aggregate reports peer mail servers send this nest about its own domains.
//!
//! Authority for behavior: `docs/goal/behavior/dmarc-reporting.md` § Admin pane
//! summary surface (§ What the page shows, § Where logic lives, § Live
//! refresh) and § Wire shapes (`list_dmarc_aggregate_reports` /
//! `get_dmarc_aggregate_report`, both Admin-class). Authority for UX/IDs:
//! `tests/e2e-unified/ui.yaml` `admin-mail-reports`.
//!
//! Per priority #2 the window totals (the shared
//! [`fauna_mail::dmarc_report::WindowSummary`] fold), the shares, the merged top
//! lists, the domain filter's options and the raw view's gunzip all live here —
//! the UI renders [`DmarcReportsSnapshot`] and dispatches [`DmarcReportsAction`];
//! the per-app glue implements one WS-RPC seam ([`DmarcReportsNest`]) over
//! `MailAdminClient` (`libs/fauna-client-bridges`). Mirrors `forwarders.rs`.
//!
//! The page re-reads on open, on a filter change and on its refresh button;
//! there is no push (§ Live refresh — decided 2026-10-09).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fauna_mail::dmarc_report::{WindowSummary, gunzip_xml, share_percent};
use fauna_protocol::MaybeSendSync;
use fauna_protocol::bridge_routing::{
    DmarcAggregateReportEntry, GetDmarcAggregateReportReply, ListDmarcAggregateReportsReply,
    ReportCountEntry,
};
use serde::{Deserialize, Serialize};

use crate::error::{DispatchError, NestError};

/// The window picker's choices, in days, in render order — 7 is the default
/// (`dmarc-reporting.md` § What the page shows → *Window and domain filter*).
pub const DMARC_REPORT_WINDOW_DAYS: [u32; 3] = [7, 30, 90];

/// The window the page opens on.
pub const DMARC_REPORT_DEFAULT_WINDOW_DAYS: u32 = 7;

/// The window picker's options, in days (`admin-mail-reports-window-select`).
/// Exported over UniFFI and wasm so no app hard-codes its own list.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn dmarc_report_window_options() -> Vec<u32> {
    DMARC_REPORT_WINDOW_DAYS.to_vec()
}

/// One `(value, messages)` row of a top list (`admin-mail-reports-source` /
/// `-sender`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DmarcCountRow {
    /// The source IP verbatim (no PTR lookup — a render never queries DNS), or
    /// the header-From domain.
    pub value: String,
    pub count: u64,
}

impl From<ReportCountEntry> for DmarcCountRow {
    fn from(e: ReportCountEntry) -> Self {
        Self {
            value: e.value,
            count: e.count,
        }
    }
}

/// The window totals (`admin-mail-reports-summary`): the shared
/// [`WindowSummary`] fold plus each alignment count's share of the messages
/// reported, so no app computes a percentage of its own.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DmarcWindowTotals {
    /// `admin-mail-reports-messages`: messages reported · reports · reporting peers.
    pub message_count: u64,
    pub report_count: u64,
    pub submitter_count: u64,
    /// `admin-mail-reports-alignment`: each a count and a whole-number share of
    /// `message_count`.
    pub dmarc_pass: u64,
    pub dmarc_pass_percent: u32,
    pub dkim_pass: u64,
    pub dkim_pass_percent: u32,
    pub spf_pass: u64,
    pub spf_pass_percent: u32,
    /// `admin-mail-reports-dispositions`.
    pub disposition_none: u64,
    pub disposition_quarantine: u64,
    pub disposition_reject: u64,
}

impl From<&WindowSummary> for DmarcWindowTotals {
    fn from(w: &WindowSummary) -> Self {
        let pct = |n| share_percent(n, w.message_count);
        Self {
            message_count: w.message_count,
            report_count: w.report_count,
            submitter_count: w.submitter_count,
            dmarc_pass: w.dmarc_pass,
            dmarc_pass_percent: pct(w.dmarc_pass),
            dkim_pass: w.dkim_pass,
            dkim_pass_percent: pct(w.dkim_pass),
            spf_pass: w.spf_pass,
            spf_pass_percent: pct(w.spf_pass),
            disposition_none: w.disposition_none,
            disposition_quarantine: w.disposition_quarantine,
            disposition_reject: w.disposition_reject,
        }
    }
}

/// One report row (`admin-mail-reports-report`), newest received first. The
/// three key fields are carried verbatim into [`DmarcReportsAction::OpenReport`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DmarcReportRowView {
    pub our_domain: String,
    pub submitter: String,
    pub report_id: String,
    /// The report period, epoch seconds (RFC 7489 `date_range`).
    pub begin_at: i64,
    pub end_at: i64,
    pub received_at: i64,
    pub message_count: u64,
    pub dmarc_pass: u64,
}

impl From<&DmarcAggregateReportEntry> for DmarcReportRowView {
    fn from(e: &DmarcAggregateReportEntry) -> Self {
        Self {
            our_domain: e.our_domain.clone(),
            submitter: e.submitter.clone(),
            report_id: e.report_id.clone(),
            begin_at: e.begin_at,
            end_at: e.end_at,
            received_at: e.received_at,
            message_count: e.summary.message_count,
            dmarc_pass: e.summary.dmarc_pass,
        }
    }
}

/// The report a row's view button opened (`admin-mail-reports-raw`): its key
/// and its XML, gunzipped here. A report that is not UTF-8 is shown with the
/// invalid bytes replaced — the raw view is for reading, never re-parsing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DmarcOpenedReport {
    pub our_domain: String,
    pub submitter: String,
    pub report_id: String,
    pub xml: String,
}

/// Coarse machine status for spinner / disabled-control rendering. Mirrors
/// `ForwarderStatus`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum DmarcReportsStatus {
    Idle,
    Loading,
}

/// Read-only snapshot the per-app UI renders for `admin-mail-reports`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DmarcReportsSnapshot {
    /// The window in force (`admin-mail-reports-window-select`), one of
    /// [`dmarc_report_window_options`].
    pub window_days: u32,
    /// The domain filter in force (`admin-mail-reports-domain-select`); `None`
    /// is every domain.
    pub our_domain: Option<String>,
    /// The active hosted local domains — the domain filter's options besides
    /// "every domain".
    pub local_domains: Vec<String>,
    /// `true` once a list read has answered. The totals, lists and empty state
    /// paint only then: "no reports" and "not read yet" are different facts.
    pub loaded: bool,
    pub totals: DmarcWindowTotals,
    /// Top source IPs and header-From domains by reported messages, ten largest.
    pub sources: Vec<DmarcCountRow>,
    pub senders: Vec<DmarcCountRow>,
    pub reports: Vec<DmarcReportRowView>,
    /// The nest's page cap was hit: the totals cover only the newest reports
    /// (`admin-mail-reports-truncated`).
    pub truncated: bool,
    /// The report a view button opened, until the filter changes.
    pub opened: Option<DmarcOpenedReport>,
    pub status: DmarcReportsStatus,
    /// Last action's error, surfaced on the page's `error-message`.
    pub error: Option<String>,
}

impl DmarcReportsSnapshot {
    fn empty() -> Self {
        Self {
            window_days: DMARC_REPORT_DEFAULT_WINDOW_DAYS,
            our_domain: None,
            local_domains: Vec::new(),
            loaded: false,
            totals: DmarcWindowTotals::default(),
            sources: Vec::new(),
            senders: Vec::new(),
            reports: Vec::new(),
            truncated: false,
            opened: None,
            status: DmarcReportsStatus::Idle,
            error: None,
        }
    }

    /// Fold one list reply into the snapshot.
    fn apply_list(&mut self, reply: ListDmarcAggregateReportsReply) {
        let window = WindowSummary::fold(&reply.reports);
        self.totals = DmarcWindowTotals::from(&window);
        self.sources = window.top_source_ips.into_iter().map(Into::into).collect();
        self.senders = window.top_header_from.into_iter().map(Into::into).collect();
        self.reports = reply.reports.iter().map(DmarcReportRowView::from).collect();
        self.truncated = reply.truncated;
        self.loaded = true;
    }
}

/// Actions the per-app UI dispatches.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum DmarcReportsAction {
    /// Re-read the window and the domain list (page open / refresh button).
    Refresh,
    /// Pick the window (one of [`dmarc_report_window_options`]) and re-read.
    SetWindow { days: u32 },
    /// Pick one local domain, or every domain (`None`), and re-read.
    SetDomain { domain: Option<String> },
    /// Open one report's XML into the raw view — the key a
    /// [`DmarcReportRowView`] carries.
    OpenReport {
        our_domain: String,
        submitter: String,
        report_id: String,
    },
}

/// WS-RPC seam to nest. Per-app glue implements this over `MailAdminClient`
/// (`libs/fauna-client-bridges`) — each method a 1:1 forward.
// Dual `async_trait` arm + `MaybeSendSync` supertrait so the one seam serves
// native + wasm (see `fauna_protocol::MaybeSendSync`).
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait DmarcReportsNest: MaybeSendSync {
    /// `fauna.bridges.list_dmarc_aggregate_reports`.
    async fn list_reports(
        &self,
        window_days: u32,
        our_domain: Option<String>,
    ) -> Result<ListDmarcAggregateReportsReply, NestError>;
    /// `fauna.bridges.get_dmarc_aggregate_report`.
    async fn get_report(
        &self,
        our_domain: String,
        submitter: String,
        report_id: String,
    ) -> Result<GetDmarcAggregateReportReply, NestError>;
    /// Active hosted local-domain names for the domain filter. Glue maps
    /// `fauna.bridges.list_local_domains`'s active rows to their `domain_name`.
    async fn list_local_domains(&self) -> Result<Vec<String>, NestError>;
}

/// One instance per admin client. Holds the rendered snapshot; drives the seam.
/// Mirrors `ForwarderMachine`.
#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct DmarcReportsMachine {
    nest: Arc<dyn DmarcReportsNest>,
    inner: Mutex<DmarcReportsSnapshot>,
}

impl DmarcReportsMachine {
    pub fn new(nest: Arc<dyn DmarcReportsNest>) -> Self {
        Self {
            nest,
            inner: Mutex::new(DmarcReportsSnapshot::empty()),
        }
    }

    fn set_status(&self, status: DmarcReportsStatus) {
        self.inner.lock().expect("snapshot mutex").status = status;
    }

    async fn refresh(&self) -> Result<(), DispatchError> {
        self.set_status(DmarcReportsStatus::Loading);
        let (window_days, our_domain) = {
            let snap = self.inner.lock().expect("snapshot mutex");
            (snap.window_days, snap.our_domain.clone())
        };
        // Both reads before taking the lock (no .await while holding it).
        let local_domains = self.nest.list_local_domains().await?;
        let reply = self.nest.list_reports(window_days, our_domain).await?;
        let mut snap = self.inner.lock().expect("snapshot mutex");
        snap.local_domains = local_domains;
        snap.apply_list(reply);
        snap.status = DmarcReportsStatus::Idle;
        Ok(())
    }

    async fn set_window(&self, days: u32) -> Result<(), DispatchError> {
        if !DMARC_REPORT_WINDOW_DAYS.contains(&days) {
            return Err(DispatchError::InvalidState(format!(
                "no {days}-day window: the page offers {DMARC_REPORT_WINDOW_DAYS:?}"
            )));
        }
        {
            let mut snap = self.inner.lock().expect("snapshot mutex");
            snap.window_days = days;
            snap.opened = None;
        }
        self.refresh().await
    }

    async fn set_domain(&self, domain: Option<String>) -> Result<(), DispatchError> {
        {
            let mut snap = self.inner.lock().expect("snapshot mutex");
            snap.our_domain = domain.filter(|d| !d.is_empty());
            snap.opened = None;
        }
        self.refresh().await
    }

    async fn open_report(
        &self,
        our_domain: String,
        submitter: String,
        report_id: String,
    ) -> Result<(), DispatchError> {
        self.set_status(DmarcReportsStatus::Loading);
        let reply = self
            .nest
            .get_report(our_domain.clone(), submitter.clone(), report_id.clone())
            .await?;
        let xml = gunzip_xml(&reply.payload_xml_gz)
            .map_err(|e| DispatchError::InvalidState(e.to_string()))?;
        let mut snap = self.inner.lock().expect("snapshot mutex");
        snap.opened = Some(DmarcOpenedReport {
            our_domain,
            submitter,
            report_id,
            xml: String::from_utf8_lossy(&xml).into_owned(),
        });
        snap.status = DmarcReportsStatus::Idle;
        Ok(())
    }
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
impl DmarcReportsMachine {
    pub fn snapshot(&self) -> DmarcReportsSnapshot {
        fauna_core::clone_locked(&self.inner, |s| s)
    }
}

#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl DmarcReportsMachine {
    /// Initial page load.
    pub async fn hydrate(&self) -> Result<(), DispatchError> {
        self.refresh().await
    }

    pub async fn dispatch(&self, action: DmarcReportsAction) -> Result<(), DispatchError> {
        // Clear any prior error before the new action runs.
        self.inner.lock().expect("snapshot mutex").error = None;
        crate::dispatch_capturing_error!(
            self,
            DmarcReportsStatus,
            match action {
                DmarcReportsAction::Refresh => self.refresh().await,
                DmarcReportsAction::SetWindow { days } => self.set_window(days).await,
                DmarcReportsAction::SetDomain { domain } => self.set_domain(domain).await,
                DmarcReportsAction::OpenReport {
                    our_domain,
                    submitter,
                    report_id,
                } => self.open_report(our_domain, submitter, report_id).await,
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::bridge_routing::DmarcAggregateSummary;
    use std::sync::Mutex as StdMutex;

    fn count(value: &str, count: u64) -> ReportCountEntry {
        ReportCountEntry {
            value: value.into(),
            count,
            extra: Default::default(),
        }
    }

    fn report(domain: &str, submitter: &str, id: &str, messages: u64) -> DmarcAggregateReportEntry {
        DmarcAggregateReportEntry {
            our_domain: domain.into(),
            submitter: submitter.into(),
            report_id: id.into(),
            begin_at: 1_760_000_000,
            end_at: 1_760_086_400,
            received_at: 1_760_090_000,
            summary: DmarcAggregateSummary {
                message_count: messages,
                dmarc_pass: messages - 1,
                dkim_pass: messages - 1,
                spf_pass: messages,
                disposition_none: messages - 1,
                disposition_reject: 1,
                top_source_ips: vec![count("192.0.2.1", messages)],
                top_header_from: vec![count(domain, messages)],
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// In-memory nest: lists the stored reports filtered by domain, records
    /// every list call's window, serves each report's XML gzipped.
    #[derive(Default)]
    struct FakeNest {
        reports: Vec<DmarcAggregateReportEntry>,
        domains: Vec<String>,
        truncated: bool,
        list_calls: StdMutex<Vec<(u32, Option<String>)>>,
        fail_list: bool,
    }

    #[async_trait]
    impl DmarcReportsNest for FakeNest {
        async fn list_reports(
            &self,
            window_days: u32,
            our_domain: Option<String>,
        ) -> Result<ListDmarcAggregateReportsReply, NestError> {
            self.list_calls
                .lock()
                .unwrap()
                .push((window_days, our_domain.clone()));
            if self.fail_list {
                return Err(NestError::Rejected("fauna.bridges.forbidden".into()));
            }
            Ok(ListDmarcAggregateReportsReply {
                reports: self
                    .reports
                    .iter()
                    .filter(|r| our_domain.as_ref().is_none_or(|d| &r.our_domain == d))
                    .cloned()
                    .collect(),
                truncated: self.truncated,
                ..Default::default()
            })
        }

        async fn get_report(
            &self,
            our_domain: String,
            submitter: String,
            report_id: String,
        ) -> Result<GetDmarcAggregateReportReply, NestError> {
            let r = self
                .reports
                .iter()
                .find(|r| {
                    r.our_domain == our_domain
                        && r.submitter == submitter
                        && r.report_id == report_id
                })
                .ok_or_else(|| NestError::Rejected("fauna.bridges.not_found".into()))?;
            Ok(GetDmarcAggregateReportReply {
                report: r.clone(),
                payload_xml_gz: fauna_mail::dmarc_report::gzip_xml(
                    format!(
                        "<feedback><report_id>{}</report_id></feedback>",
                        r.report_id
                    )
                    .as_bytes(),
                ),
                extra: Default::default(),
            })
        }

        async fn list_local_domains(&self) -> Result<Vec<String>, NestError> {
            Ok(self.domains.clone())
        }
    }

    fn machine(nest: FakeNest) -> (DmarcReportsMachine, Arc<FakeNest>) {
        let nest = Arc::new(nest);
        (DmarcReportsMachine::new(nest.clone()), nest)
    }

    fn two_domains() -> FakeNest {
        FakeNest {
            reports: vec![
                report("a.test", "Peer <r@peer.test>", "r-1", 10),
                report("b.test", "Other <r@other.test>", "r-2", 4),
            ],
            domains: vec!["a.test".into(), "b.test".into()],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn refresh_folds_totals_lists_and_rows_on_the_default_window() {
        let (m, nest) = machine(two_domains());
        m.dispatch(DmarcReportsAction::Refresh).await.unwrap();
        let s = m.snapshot();
        assert_eq!(nest.list_calls.lock().unwrap().as_slice(), &[(7, None)]);
        assert!(s.loaded);
        assert_eq!(s.window_days, 7);
        assert_eq!(s.local_domains, vec!["a.test", "b.test"]);
        assert_eq!(
            (
                s.totals.message_count,
                s.totals.report_count,
                s.totals.submitter_count
            ),
            (14, 2, 2)
        );
        assert_eq!((s.totals.dmarc_pass, s.totals.dmarc_pass_percent), (12, 86));
        assert_eq!((s.totals.spf_pass, s.totals.spf_pass_percent), (14, 100));
        assert_eq!(s.totals.disposition_reject, 2);
        assert_eq!(
            s.sources,
            vec![DmarcCountRow {
                value: "192.0.2.1".into(),
                count: 14
            }]
        );
        assert_eq!(s.senders.len(), 2);
        assert_eq!(s.reports.len(), 2);
        assert_eq!(s.reports[0].report_id, "r-1");
        assert_eq!(
            (s.reports[0].message_count, s.reports[0].dmarc_pass),
            (10, 9)
        );
        assert!(!s.truncated);
        assert!(s.error.is_none());
    }

    #[tokio::test]
    async fn an_unread_page_is_not_an_empty_one() {
        let (m, _) = machine(FakeNest::default());
        assert!(!m.snapshot().loaded);
        m.dispatch(DmarcReportsAction::Refresh).await.unwrap();
        let s = m.snapshot();
        assert!(s.loaded);
        assert!(s.reports.is_empty());
        assert_eq!(s.totals, DmarcWindowTotals::default());
    }

    #[tokio::test]
    async fn filters_re_read_and_close_the_opened_report() {
        let (m, nest) = machine(two_domains());
        m.dispatch(DmarcReportsAction::Refresh).await.unwrap();
        m.dispatch(DmarcReportsAction::OpenReport {
            our_domain: "a.test".into(),
            submitter: "Peer <r@peer.test>".into(),
            report_id: "r-1".into(),
        })
        .await
        .unwrap();
        assert!(m.snapshot().opened.is_some());

        m.dispatch(DmarcReportsAction::SetWindow { days: 30 })
            .await
            .unwrap();
        assert!(m.snapshot().opened.is_none());
        m.dispatch(DmarcReportsAction::SetDomain {
            domain: Some("b.test".into()),
        })
        .await
        .unwrap();
        let s = m.snapshot();
        assert_eq!(s.our_domain.as_deref(), Some("b.test"));
        assert_eq!(s.reports.len(), 1);
        assert_eq!(s.totals.message_count, 4);
        m.dispatch(DmarcReportsAction::SetDomain { domain: None })
            .await
            .unwrap();
        assert_eq!(
            nest.list_calls.lock().unwrap().as_slice(),
            &[
                (7, None),
                (30, None),
                (30, Some("b.test".into())),
                (30, None)
            ]
        );
    }

    #[tokio::test]
    async fn a_window_off_the_picker_is_refused_without_a_read() {
        let (m, nest) = machine(two_domains());
        assert!(
            m.dispatch(DmarcReportsAction::SetWindow { days: 365 })
                .await
                .is_err()
        );
        let s = m.snapshot();
        assert_eq!(s.window_days, 7);
        assert!(s.error.is_some());
        assert!(nest.list_calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn open_report_gunzips_the_archive_into_the_raw_view() {
        let (m, _) = machine(two_domains());
        m.dispatch(DmarcReportsAction::OpenReport {
            our_domain: "b.test".into(),
            submitter: "Other <r@other.test>".into(),
            report_id: "r-2".into(),
        })
        .await
        .unwrap();
        let opened = m.snapshot().opened.unwrap();
        assert_eq!(opened.report_id, "r-2");
        assert_eq!(
            opened.xml,
            "<feedback><report_id>r-2</report_id></feedback>"
        );
    }

    #[tokio::test]
    async fn truncation_and_read_failures_reach_the_snapshot() {
        let (m, _) = machine(FakeNest {
            truncated: true,
            ..two_domains()
        });
        m.dispatch(DmarcReportsAction::Refresh).await.unwrap();
        assert!(m.snapshot().truncated);

        let (m, _) = machine(FakeNest {
            fail_list: true,
            ..two_domains()
        });
        assert!(m.dispatch(DmarcReportsAction::Refresh).await.is_err());
        let s = m.snapshot();
        assert!(!s.loaded);
        assert_eq!(s.status, DmarcReportsStatus::Idle);
        assert!(s.error.is_some());
    }
}
