//! The admin Mail reports sub-page (`admin-mail-reports`) — the DMARC aggregate
//! reports peer mail servers send this nest about its own domains
//! (`dmarc-reporting.md` § Admin pane summary surface).
//!
//! A dumb renderer of the shared `DmarcReportsMachine`
//! (`fauna-client-mail-settings::dmarc_reports`): the totals, the shares, the
//! merged top lists and the raw view's gunzip are all the machine's; this file
//! only words them. The shell (`super`) owns the machine, the ops and the fold;
//! errors ride the app-wide `error-message` (ui.yaml `errors`).
//!
//! The three indexed components (`-source`, `-sender`, `-report`) repeat their
//! container id per row with every leaf scoped `within` it by position — the
//! convention-1 shape `custody_hosting.rs` uses. The conditional texts
//! (`-truncated`, `-empty`, `-raw`) are painted only on their own path, and
//! nothing but the filters paints before the first read answers: "no reports"
//! and "not read yet" are different facts.

use fauna_client_mail_settings::dmarc_reports::{
    DmarcCountRow, DmarcReportRowView, DmarcReportsSnapshot, dmarc_report_window_options,
};
use fauna_i18n::strings::admin::mail_reports_page as t;
use fauna_ui_ids as ids;

use super::{Action, AdminState};
use crate::element::{Element, Gesture, SelectTarget};
use crate::pages::Page;

fn count_rows(
    container: &str,
    value_id: &str,
    count_id: &str,
    rows: &[DmarcCountRow],
) -> Vec<Element> {
    let mut els = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        els.push(Element::label(container, String::new()));
        els.push(Element::label(value_id, row.value.clone()).within(container, i));
        els.push(
            Element::label(count_id, t::row_messages(&row.count.to_string())).within(container, i),
        );
    }
    els
}

fn report_row(i: usize, r: &DmarcReportRowView) -> Vec<Element> {
    let date = fauna_core::format::format_unix_local_date;
    let children = [
        Element::label(
            ids::ADMIN_MAIL_REPORTS_REPORT_SUBMITTER,
            r.submitter.clone(),
        ),
        Element::label(ids::ADMIN_MAIL_REPORTS_REPORT_DOMAIN, r.our_domain.clone()),
        Element::label(
            ids::ADMIN_MAIL_REPORTS_REPORT_PERIOD,
            t::report_period(&date(r.begin_at), &date(r.end_at)),
        ),
        Element::label(
            ids::ADMIN_MAIL_REPORTS_REPORT_MESSAGES,
            t::row_messages(&r.message_count.to_string()),
        ),
        Element::label(
            ids::ADMIN_MAIL_REPORTS_REPORT_PASS,
            t::report_pass(&r.dmarc_pass.to_string(), &r.message_count.to_string()),
        ),
        Element::gesture_button(
            ids::ADMIN_MAIL_REPORTS_REPORT_VIEW_BUTTON,
            t::VIEW,
            true,
            Gesture::Admin(Action::OpenMailReport {
                our_domain: r.our_domain.clone(),
                submitter: r.submitter.clone(),
                report_id: r.report_id.clone(),
            }),
        ),
    ];
    let mut els = vec![Element::label(
        ids::ADMIN_MAIL_REPORTS_REPORT,
        String::new(),
    )];
    els.extend(
        children
            .into_iter()
            .map(|e| e.within(ids::ADMIN_MAIL_REPORTS_REPORT, i)),
    );
    els
}

fn filters(snap: Option<&DmarcReportsSnapshot>) -> Vec<Element> {
    let window = snap.map_or(
        fauna_client_mail_settings::dmarc_reports::DMARC_REPORT_DEFAULT_WINDOW_DAYS,
        |s| s.window_days,
    );
    let windows = dmarc_report_window_options();
    // The domain picker's raw values: "" is every domain, then each hosted one.
    let domains: Vec<String> = std::iter::once(String::new())
        .chain(snap.map(|s| s.local_domains.clone()).unwrap_or_default())
        .collect();
    let domain = snap.and_then(|s| s.our_domain.clone()).unwrap_or_default();
    let domain_label = |d: &str| {
        if d.is_empty() {
            t::EVERY_DOMAIN.to_string()
        } else {
            d.to_string()
        }
    };
    vec![
        Element::select(
            ids::ADMIN_MAIL_REPORTS_WINDOW_SELECT,
            window.to_string(),
            SelectTarget::MailReportsWindow,
            windows.iter().map(u32::to_string).collect(),
        )
        .display_value(t::window_days(&window.to_string()))
        .option_labels(
            windows
                .iter()
                .map(|d| t::window_days(&d.to_string()))
                .collect(),
        )
        .labelled(t::WINDOW_LABEL),
        Element::select(
            ids::ADMIN_MAIL_REPORTS_DOMAIN_SELECT,
            domain.clone(),
            SelectTarget::MailReportsDomain,
            domains.clone(),
        )
        .display_value(domain_label(&domain))
        .option_labels(domains.iter().map(|d| domain_label(d)).collect())
        .labelled(t::DOMAIN_LABEL),
        Element::gesture_button(
            ids::ADMIN_MAIL_REPORTS_REFRESH_BUTTON,
            t::REFRESH,
            true,
            Gesture::Admin(Action::RefreshMailReports),
        ),
    ]
}

pub(super) fn mail_reports_elements(state: &AdminState) -> Vec<Element> {
    let snap = state.dmarc_reports_snapshot.as_deref();
    let mut els = vec![
        Element::label(ids::PAGE_HEADING, t::TITLE),
        Element::chrome(t::DESCRIPTION),
        Element::gesture_button(
            ids::ADMIN_NAV_BACK,
            super::t::EXIT,
            true,
            Gesture::Nav(Page::Conversations),
        )
        .nav_back(),
    ];
    els.extend(filters(snap));

    let Some(snap) = snap.filter(|s| s.loaded) else {
        return els;
    };

    let totals = &snap.totals;
    els.push(Element::label(
        ids::ADMIN_MAIL_REPORTS_SUMMARY,
        t::SUMMARY_TITLE,
    ));
    els.push(Element::label(
        ids::ADMIN_MAIL_REPORTS_MESSAGES,
        t::messages(
            &totals.message_count.to_string(),
            &totals.report_count.to_string(),
            &totals.submitter_count.to_string(),
        ),
    ));
    els.push(Element::label(
        ids::ADMIN_MAIL_REPORTS_ALIGNMENT,
        t::alignment(
            &totals.dmarc_pass.to_string(),
            &totals.dmarc_pass_percent.to_string(),
            &totals.dkim_pass.to_string(),
            &totals.dkim_pass_percent.to_string(),
            &totals.spf_pass.to_string(),
            &totals.spf_pass_percent.to_string(),
        ),
    ));
    els.push(Element::label(
        ids::ADMIN_MAIL_REPORTS_DISPOSITIONS,
        t::dispositions(
            &totals.disposition_none.to_string(),
            &totals.disposition_quarantine.to_string(),
            &totals.disposition_reject.to_string(),
        ),
    ));
    if snap.truncated {
        els.push(Element::label(
            ids::ADMIN_MAIL_REPORTS_TRUNCATED,
            t::TRUNCATED,
        ));
    }
    if snap.reports.is_empty() {
        els.push(Element::label(ids::ADMIN_MAIL_REPORTS_EMPTY, t::EMPTY));
    }

    if !snap.sources.is_empty() {
        els.push(Element::chrome(t::SOURCES_TITLE));
        els.extend(count_rows(
            ids::ADMIN_MAIL_REPORTS_SOURCE,
            ids::ADMIN_MAIL_REPORTS_SOURCE_IP,
            ids::ADMIN_MAIL_REPORTS_SOURCE_COUNT,
            &snap.sources,
        ));
    }
    if !snap.senders.is_empty() {
        els.push(Element::chrome(t::SENDERS_TITLE));
        els.extend(count_rows(
            ids::ADMIN_MAIL_REPORTS_SENDER,
            ids::ADMIN_MAIL_REPORTS_SENDER_DOMAIN,
            ids::ADMIN_MAIL_REPORTS_SENDER_COUNT,
            &snap.senders,
        ));
    }
    if !snap.reports.is_empty() {
        els.push(Element::chrome(t::REPORTS_TITLE));
        for (i, r) in snap.reports.iter().enumerate() {
            els.extend(report_row(i, r));
        }
    }

    if let Some(opened) = snap.opened.as_ref() {
        els.push(Element::chrome(t::RAW_TITLE));
        els.push(Element::label(
            ids::ADMIN_MAIL_REPORTS_RAW,
            opened.xml.clone(),
        ));
    }

    els
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_mail_settings::dmarc_reports::{
        DmarcOpenedReport, DmarcReportsStatus, DmarcWindowTotals,
    };

    fn snapshot(reports: Vec<DmarcReportRowView>) -> DmarcReportsSnapshot {
        DmarcReportsSnapshot {
            window_days: 30,
            our_domain: None,
            local_domains: vec!["a.test".into()],
            loaded: true,
            totals: DmarcWindowTotals {
                message_count: 10,
                report_count: reports.len() as u64,
                submitter_count: 1,
                dmarc_pass: 9,
                dmarc_pass_percent: 90,
                ..Default::default()
            },
            sources: vec![DmarcCountRow {
                value: "192.0.2.1".into(),
                count: 10,
            }],
            senders: vec![DmarcCountRow {
                value: "a.test".into(),
                count: 10,
            }],
            reports,
            truncated: false,
            opened: None,
            status: DmarcReportsStatus::Idle,
            error: None,
        }
    }

    fn row() -> DmarcReportRowView {
        DmarcReportRowView {
            our_domain: "a.test".into(),
            submitter: "Peer <r@peer.test>".into(),
            report_id: "r-1".into(),
            begin_at: 1_760_000_000,
            end_at: 1_760_086_400,
            received_at: 1_760_090_000,
            message_count: 10,
            dmarc_pass: 9,
        }
    }

    fn state_with(snap: Option<DmarcReportsSnapshot>) -> AdminState {
        AdminState {
            dmarc_reports_snapshot: snap.map(Box::new),
            ..Default::default()
        }
    }

    fn ids_of(els: &[Element]) -> Vec<&str> {
        els.iter()
            .map(|e| e.id.as_str())
            .filter(|id| !id.is_empty())
            .collect()
    }

    fn text_of<'a>(els: &'a [Element], id: &str) -> &'a str {
        els.iter()
            .find(|e| e.id == id)
            .map(|e| e.text.as_str())
            .unwrap_or_else(|| panic!("{id} not painted"))
    }

    #[test]
    fn before_the_first_read_only_the_filters_paint() {
        let els = mail_reports_elements(&state_with(None));
        assert_eq!(
            ids_of(&els),
            vec![
                ids::PAGE_HEADING,
                ids::ADMIN_NAV_BACK,
                ids::ADMIN_MAIL_REPORTS_WINDOW_SELECT,
                ids::ADMIN_MAIL_REPORTS_DOMAIN_SELECT,
                ids::ADMIN_MAIL_REPORTS_REFRESH_BUTTON,
            ]
        );
        assert_eq!(text_of(&els, ids::ADMIN_MAIL_REPORTS_WINDOW_SELECT), "7");
    }

    #[test]
    fn a_loaded_window_paints_totals_lists_and_indexed_rows() {
        let els = mail_reports_elements(&state_with(Some(snapshot(vec![row(), row()]))));
        let ids = ids_of(&els);
        for id in [
            ids::ADMIN_MAIL_REPORTS_SUMMARY,
            ids::ADMIN_MAIL_REPORTS_MESSAGES,
            ids::ADMIN_MAIL_REPORTS_ALIGNMENT,
            ids::ADMIN_MAIL_REPORTS_DISPOSITIONS,
            ids::ADMIN_MAIL_REPORTS_SOURCE_IP,
            ids::ADMIN_MAIL_REPORTS_SENDER_DOMAIN,
        ] {
            assert!(ids.contains(&id), "{id} missing");
        }
        for absent in [
            ids::ADMIN_MAIL_REPORTS_EMPTY,
            ids::ADMIN_MAIL_REPORTS_TRUNCATED,
            ids::ADMIN_MAIL_REPORTS_RAW,
        ] {
            assert!(!ids.contains(&absent), "{absent} painted");
        }
        assert_eq!(
            ids.iter()
                .filter(|i| **i == ids::ADMIN_MAIL_REPORTS_REPORT_VIEW_BUTTON)
                .count(),
            2
        );
        assert_eq!(
            text_of(&els, ids::ADMIN_MAIL_REPORTS_MESSAGES),
            t::messages("10", "2", "1")
        );
        assert!(text_of(&els, ids::ADMIN_MAIL_REPORTS_ALIGNMENT).contains("(90%)"));
        assert_eq!(
            text_of(&els, ids::ADMIN_MAIL_REPORTS_REPORT_PASS),
            t::report_pass("9", "10")
        );
        assert_eq!(text_of(&els, ids::ADMIN_MAIL_REPORTS_WINDOW_SELECT), "30");
    }

    #[test]
    fn the_empty_truncated_and_raw_paths_paint_their_own_text() {
        let mut snap = snapshot(vec![]);
        snap.sources.clear();
        snap.senders.clear();
        let els = mail_reports_elements(&state_with(Some(snap.clone())));
        assert_eq!(text_of(&els, ids::ADMIN_MAIL_REPORTS_EMPTY), t::EMPTY);

        let mut snap = snapshot(vec![row()]);
        snap.truncated = true;
        snap.opened = Some(DmarcOpenedReport {
            our_domain: "a.test".into(),
            submitter: "Peer <r@peer.test>".into(),
            report_id: "r-1".into(),
            xml: "<feedback/>".into(),
        });
        let els = mail_reports_elements(&state_with(Some(snap)));
        assert_eq!(
            text_of(&els, ids::ADMIN_MAIL_REPORTS_TRUNCATED),
            t::TRUNCATED
        );
        assert_eq!(text_of(&els, ids::ADMIN_MAIL_REPORTS_RAW), "<feedback/>");
        assert!(!ids_of(&els).contains(&ids::ADMIN_MAIL_REPORTS_EMPTY));
    }
}
