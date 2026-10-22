//! The admin **Mail reports** sub-page (`admin-mail-reports`) — the DMARC
//! aggregate reports peer mail servers send this nest about its own domains
//! (`docs/goal/behavior/dmarc-reporting.md` § Admin pane summary surface; ids in
//! `tests/e2e-unified/ui.yaml` `admin-mail-reports`).
//!
//! A dumb renderer of the shared [`DmarcReportsMachine`]
//! (`fauna_client_mail_settings::dmarc_reports`): the window totals, the shares,
//! the merged top lists and the raw view's gunzip are all the machine's; this
//! file only words them (priority #2/#4; the lead app is
//! `apps/fauna-tui/src/admin/mail_reports.rs`, whose contract this follows).
//!
//! * Before the first read answers (`snapshot.loaded`) only the heading, the two
//!   filters and the refresh button paint — "no reports" and "not read yet" are
//!   different facts. The back button is the admin rail's shared
//!   `admin-nav-back`.
//! * Once loaded: `-summary`, `-messages`, `-alignment`, `-dispositions` paint;
//!   `-empty` only when no report is listed, `-truncated` only while the nest's
//!   page cap was hit, `-raw` only while a report is opened.
//! * The indexed components (`-source`, `-sender`, `-report`) repeat their
//!   container id once per row with the leaves inside it, in document order; a
//!   report's view button carries that row's own key in its handler.
//! * The pickers expose raw values to the e2e agent (`set_test_text` = the day
//!   count / the domain, `""` for every domain; `set_test_option_keys` = the
//!   same tokens), the painted labels being the localized ones.
//! * The page re-reads on open (every time it is mapped), on a filter change and
//!   on refresh; there is no push.
//!
//! Registered as an admin-shell `gtk::Stack` sub-page (child `admin-mail-reports`)
//! in `views/admin.rs`, reached through the nav rail / state protocol.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use fauna_client_mail_settings::dmarc_reports::{
    DMARC_REPORT_DEFAULT_WINDOW_DAYS, DmarcCountRow, DmarcReportRowView, DmarcReportsAction,
    DmarcReportsSnapshot, dmarc_report_window_options,
};
use fauna_ui_ids as ids;

use crate::async_helper::spawn_with_snapshot;
use crate::i18n::strings::admin::mail_reports_page as t;
use crate::testid::{set_test_id, set_test_option_keys, set_test_text};

/// Dispatches one machine action (and re-renders the answer).
type Dispatch = Rc<dyn Fn(DmarcReportsAction)>;

/// Handles to the widgets a snapshot renders into. GTK objects are
/// reference-counted, so cloning shares the widgets.
#[derive(Clone)]
struct Widgets {
    error: gtk::Label,
    window: gtk::DropDown,
    domain: gtk::DropDown,
    refresh: gtk::Button,
    /// Everything below the filters; rebuilt on every render.
    content: gtk::Box,
    /// Set while `render` programmatically updates a picker, so its change
    /// handler does not echo the update back as an action.
    syncing: Rc<Cell<bool>>,
}

fn margin_box(orientation: gtk::Orientation, spacing: i32) -> gtk::Box {
    let b = gtk::Box::new(orientation, spacing);
    b.set_margin_start(12);
    b.set_margin_end(12);
    b.set_margin_top(2);
    b.set_margin_bottom(2);
    b
}

/// A left-aligned wrapping text label carrying `id` (empty id = no test id).
fn text_label(id: &str, text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_halign(gtk::Align::Start);
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_margin_start(12);
    label.set_margin_end(12);
    label.set_margin_top(2);
    label.set_margin_bottom(2);
    if !id.is_empty() {
        set_test_id(&label, id);
    }
    label
}

/// An id-less section title.
fn section_title(text: &str) -> gtk::Label {
    let label = text_label("", text);
    label.add_css_class("heading");
    label.set_margin_top(10);
    label
}

fn domain_label(domain: &str) -> String {
    if domain.is_empty() {
        t::EVERY_DOMAIN.to_string()
    } else {
        domain.to_string()
    }
}

/// Build the page's static frame: heading, description, filters, error label and
/// the (initially empty) scrolled content area.
fn build() -> (gtk::Box, Widgets) {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let heading = gtk::Label::new(Some(t::TITLE));
    heading.add_css_class("title-2");
    heading.set_margin_top(16);
    heading.set_margin_bottom(4);
    heading.set_margin_start(12);
    heading.set_halign(gtk::Align::Start);
    set_test_id(&heading, ids::PAGE_HEADING);
    outer.append(&heading);

    let desc = gtk::Label::new(Some(t::DESCRIPTION));
    desc.add_css_class("dim-label");
    desc.set_halign(gtk::Align::Start);
    desc.set_xalign(0.0);
    desc.set_margin_start(12);
    desc.set_margin_end(12);
    desc.set_margin_bottom(8);
    desc.set_wrap(true);
    outer.append(&desc);

    // ── Filters: window, domain, refresh ──
    let filters = margin_box(gtk::Orientation::Horizontal, 8);
    let windows = dmarc_report_window_options();
    let window_labels: Vec<String> = windows
        .iter()
        .map(|d| t::window_days(&d.to_string()))
        .collect();
    let window_labels: Vec<&str> = window_labels.iter().map(String::as_str).collect();
    let window = gtk::DropDown::from_strings(&window_labels);
    window.set_valign(gtk::Align::Center);
    window.set_tooltip_text(Some(t::WINDOW_LABEL));
    let window_keys: Vec<String> = windows.iter().map(u32::to_string).collect();
    let window_keys: Vec<&str> = window_keys.iter().map(String::as_str).collect();
    set_test_option_keys(&window, &window_keys);
    if let Some(i) = windows
        .iter()
        .position(|d| *d == DMARC_REPORT_DEFAULT_WINDOW_DAYS)
    {
        window.set_selected(i as u32);
    }
    set_test_text(&window, &DMARC_REPORT_DEFAULT_WINDOW_DAYS.to_string());
    set_test_id(&window, ids::ADMIN_MAIL_REPORTS_WINDOW_SELECT);
    filters.append(&window);

    let domain = gtk::DropDown::from_strings(&[t::EVERY_DOMAIN]);
    domain.set_valign(gtk::Align::Center);
    domain.set_tooltip_text(Some(t::DOMAIN_LABEL));
    set_test_option_keys(&domain, &[""]);
    set_test_text(&domain, "");
    set_test_id(&domain, ids::ADMIN_MAIL_REPORTS_DOMAIN_SELECT);
    filters.append(&domain);

    let refresh = gtk::Button::with_label(t::REFRESH);
    refresh.set_valign(gtk::Align::Center);
    set_test_id(&refresh, ids::ADMIN_MAIL_REPORTS_REFRESH_BUTTON);
    filters.append(&refresh);
    outer.append(&filters);

    let error = gtk::Label::new(None);
    error.set_visible(false);
    error.set_wrap(true);
    error.set_halign(gtk::Align::Start);
    error.set_margin_start(12);
    error.add_css_class("error");
    set_test_id(&error, ids::ERROR_MESSAGE);
    outer.append(&error);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&content)
        .build();
    outer.append(&scrolled);

    (
        outer,
        Widgets {
            error,
            window,
            domain,
            refresh,
            content,
            syncing: Rc::new(Cell::new(false)),
        },
    )
}

/// The `-source` / `-sender` indexed rows: one container per row, value and
/// count leaves inside it.
fn count_rows(
    content: &gtk::Box,
    container_id: &str,
    value_id: &str,
    count_id: &str,
    rows: &[DmarcCountRow],
) {
    for row in rows {
        let line = margin_box(gtk::Orientation::Horizontal, 12);
        set_test_id(&line, container_id);
        let value = gtk::Label::new(Some(&row.value));
        value.set_halign(gtk::Align::Start);
        value.set_xalign(0.0);
        value.set_hexpand(true);
        value.set_selectable(true);
        value.add_css_class("monospace");
        set_test_id(&value, value_id);
        line.append(&value);
        let count = gtk::Label::new(Some(&t::row_messages(&row.count.to_string())));
        count.set_halign(gtk::Align::End);
        count.add_css_class("dim-label");
        set_test_id(&count, count_id);
        line.append(&count);
        content.append(&line);
    }
}

/// One `-report` indexed row: its leaves and the view button bound to the row's
/// own key.
fn report_row(content: &gtk::Box, r: &DmarcReportRowView, dispatch: &Dispatch) {
    let date = fauna_core::format::format_unix_local_date;
    let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
    card.set_margin_top(4);
    card.set_margin_bottom(4);
    card.add_css_class("card");
    set_test_id(&card, ids::ADMIN_MAIL_REPORTS_REPORT);
    let leaf = text_label;
    card.append(&leaf(
        ids::ADMIN_MAIL_REPORTS_REPORT_SUBMITTER,
        &r.submitter,
    ));
    card.append(&leaf(ids::ADMIN_MAIL_REPORTS_REPORT_DOMAIN, &r.our_domain));
    card.append(&leaf(
        ids::ADMIN_MAIL_REPORTS_REPORT_PERIOD,
        &t::report_period(&date(r.begin_at), &date(r.end_at)),
    ));
    card.append(&leaf(
        ids::ADMIN_MAIL_REPORTS_REPORT_MESSAGES,
        &t::row_messages(&r.message_count.to_string()),
    ));
    card.append(&leaf(
        ids::ADMIN_MAIL_REPORTS_REPORT_PASS,
        &t::report_pass(&r.dmarc_pass.to_string(), &r.message_count.to_string()),
    ));
    let view = gtk::Button::with_label(t::VIEW);
    view.set_halign(gtk::Align::End);
    view.set_margin_end(12);
    view.set_margin_bottom(4);
    set_test_id(&view, ids::ADMIN_MAIL_REPORTS_REPORT_VIEW_BUTTON);
    {
        let dispatch = Rc::clone(dispatch);
        let our_domain = r.our_domain.clone();
        let submitter = r.submitter.clone();
        let report_id = r.report_id.clone();
        view.connect_clicked(move |_| {
            dispatch(DmarcReportsAction::OpenReport {
                our_domain: our_domain.clone(),
                submitter: submitter.clone(),
                report_id: report_id.clone(),
            });
        });
    }
    card.append(&view);
    content.append(&card);
}

/// Repaint the whole page from a snapshot.
fn render(w: &Widgets, snap: &DmarcReportsSnapshot, dispatch: &Dispatch) {
    super::render_error_label(&w.error, snap.error.as_deref());

    // ── Filters: reflect the machine's window / domain without echoing ──
    w.syncing.set(true);
    let windows = dmarc_report_window_options();
    if let Some(i) = windows.iter().position(|d| *d == snap.window_days) {
        w.window.set_selected(i as u32);
    }
    set_test_text(&w.window, &snap.window_days.to_string());

    let domains: Vec<String> = std::iter::once(String::new())
        .chain(snap.local_domains.iter().cloned())
        .collect();
    let labels: Vec<String> = domains.iter().map(|d| domain_label(d)).collect();
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let key_refs: Vec<&str> = domains.iter().map(String::as_str).collect();
    w.domain.set_model(Some(&gtk::StringList::new(&label_refs)));
    set_test_option_keys(&w.domain, &key_refs);
    let current = snap.our_domain.clone().unwrap_or_default();
    let idx = domains.iter().position(|d| *d == current).unwrap_or(0);
    w.domain.set_selected(idx as u32);
    set_test_text(&w.domain, &current);
    w.syncing.set(false);

    // ── Content: rebuilt from scratch ──
    while let Some(child) = w.content.first_child() {
        w.content.remove(&child);
    }
    if !snap.loaded {
        return;
    }
    let totals = &snap.totals;
    let content = &w.content;
    let summary = section_title(t::SUMMARY_TITLE);
    set_test_id(&summary, ids::ADMIN_MAIL_REPORTS_SUMMARY);
    content.append(&summary);
    content.append(&text_label(
        ids::ADMIN_MAIL_REPORTS_MESSAGES,
        &t::messages(
            &totals.message_count.to_string(),
            &totals.report_count.to_string(),
            &totals.submitter_count.to_string(),
        ),
    ));
    content.append(&text_label(
        ids::ADMIN_MAIL_REPORTS_ALIGNMENT,
        &t::alignment(
            &totals.dmarc_pass.to_string(),
            &totals.dmarc_pass_percent.to_string(),
            &totals.dkim_pass.to_string(),
            &totals.dkim_pass_percent.to_string(),
            &totals.spf_pass.to_string(),
            &totals.spf_pass_percent.to_string(),
        ),
    ));
    content.append(&text_label(
        ids::ADMIN_MAIL_REPORTS_DISPOSITIONS,
        &t::dispositions(
            &totals.disposition_none.to_string(),
            &totals.disposition_quarantine.to_string(),
            &totals.disposition_reject.to_string(),
        ),
    ));
    if snap.truncated {
        content.append(&text_label(ids::ADMIN_MAIL_REPORTS_TRUNCATED, t::TRUNCATED));
    }
    if snap.reports.is_empty() {
        content.append(&text_label(ids::ADMIN_MAIL_REPORTS_EMPTY, t::EMPTY));
    }

    if !snap.sources.is_empty() {
        content.append(&section_title(t::SOURCES_TITLE));
        count_rows(
            content,
            ids::ADMIN_MAIL_REPORTS_SOURCE,
            ids::ADMIN_MAIL_REPORTS_SOURCE_IP,
            ids::ADMIN_MAIL_REPORTS_SOURCE_COUNT,
            &snap.sources,
        );
    }
    if !snap.senders.is_empty() {
        content.append(&section_title(t::SENDERS_TITLE));
        count_rows(
            content,
            ids::ADMIN_MAIL_REPORTS_SENDER,
            ids::ADMIN_MAIL_REPORTS_SENDER_DOMAIN,
            ids::ADMIN_MAIL_REPORTS_SENDER_COUNT,
            &snap.senders,
        );
    }
    if !snap.reports.is_empty() {
        content.append(&section_title(t::REPORTS_TITLE));
        for r in &snap.reports {
            report_row(content, r, dispatch);
        }
    }
    if let Some(opened) = snap.opened.as_ref() {
        content.append(&section_title(t::RAW_TITLE));
        let raw = text_label(ids::ADMIN_MAIL_REPORTS_RAW, &opened.xml);
        raw.set_selectable(true);
        raw.set_wrap_mode(gtk::pango::WrapMode::Char);
        raw.add_css_class("monospace");
        content.append(&raw);
    }
}

/// Build the `admin-mail-reports` page, wired to the shared machine when a
/// client is registered (the page stays a bare frame otherwise — e.g. a unit
/// test with no client).
pub fn build_admin_mail_reports_page() -> gtk::Box {
    let (outer, w) = build();
    let Some(client) = super::get_client() else {
        return outer;
    };
    let machine = Arc::new(crate::mail_glue::build_dmarc_reports_machine(&client));
    let rt = client.runtime_handle();

    // `dispatch` re-renders through itself (the view buttons it paints hold it),
    // so tie the knot through a weak late-bound cell.
    let slot: Rc<std::cell::RefCell<Option<Dispatch>>> = Rc::new(std::cell::RefCell::new(None));
    let dispatch: Dispatch = {
        let machine = Arc::clone(&machine);
        let rt = rt.clone();
        let w = w.clone();
        let slot = Rc::clone(&slot);
        Rc::new(move |action: DmarcReportsAction| {
            let machine = Arc::clone(&machine);
            let w = w.clone();
            let slot = Rc::clone(&slot);
            spawn_with_snapshot(
                &rt,
                move || async move {
                    let _ = machine.dispatch(action).await;
                    machine.snapshot()
                },
                move |snap| {
                    if let Some(d) = slot.borrow().clone() {
                        render(&w, &snap, &d);
                    }
                },
            );
        })
    };
    *slot.borrow_mut() = Some(Rc::clone(&dispatch));

    // Re-read whenever the page is shown: the sub-page outlives its first visit
    // and new reports arrive by mail.
    {
        let dispatch = Rc::clone(&dispatch);
        outer.connect_map(move |_| dispatch(DmarcReportsAction::Refresh));
    }
    {
        let dispatch = Rc::clone(&dispatch);
        w.refresh
            .connect_clicked(move |_| dispatch(DmarcReportsAction::Refresh));
    }
    {
        let dispatch = Rc::clone(&dispatch);
        let syncing = Rc::clone(&w.syncing);
        w.window.connect_selected_notify(move |dd| {
            if syncing.get() {
                return;
            }
            if let Some(days) = dmarc_report_window_options().get(dd.selected() as usize) {
                dispatch(DmarcReportsAction::SetWindow { days: *days });
            }
        });
    }
    {
        let dispatch = Rc::clone(&dispatch);
        let syncing = Rc::clone(&w.syncing);
        w.domain.connect_selected_notify(move |dd| {
            if syncing.get() {
                return;
            }
            let label = dd
                .selected_item()
                .and_then(|o| o.downcast::<gtk::StringObject>().ok())
                .map(|s| s.string().to_string())
                .unwrap_or_default();
            // The first option is "every domain"; the rest are the domains.
            let domain = (dd.selected() > 0 && !label.is_empty()).then_some(label);
            dispatch(DmarcReportsAction::SetDomain { domain });
        });
    }
    outer
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

    fn noop() -> Dispatch {
        Rc::new(|_| {})
    }

    fn present(root: &gtk::Box, id: &str) -> bool {
        crate::testid::find_by_test_id(root, id).is_some()
    }

    #[test]
    fn before_the_first_read_only_the_filters_paint() {
        crate::testid::run_on_gtk_thread(|| {
            let (page, _w) = build();
            for id in [
                ids::PAGE_HEADING,
                ids::ADMIN_MAIL_REPORTS_WINDOW_SELECT,
                ids::ADMIN_MAIL_REPORTS_DOMAIN_SELECT,
                ids::ADMIN_MAIL_REPORTS_REFRESH_BUTTON,
            ] {
                assert!(present(&page, id), "{id} missing");
            }
            assert!(!present(&page, ids::ADMIN_MAIL_REPORTS_MESSAGES));
            let window =
                crate::testid::find_by_test_id(&page, ids::ADMIN_MAIL_REPORTS_WINDOW_SELECT)
                    .expect("window select");
            assert_eq!(crate::automation::find::text_of(&window), "7");
        });
    }

    #[test]
    fn a_loaded_window_paints_totals_lists_and_indexed_rows() {
        crate::testid::run_on_gtk_thread(|| {
            let (page, w) = build();
            render(&w, &snapshot(vec![row(), row()]), &noop());
            for id in [
                ids::ADMIN_MAIL_REPORTS_SUMMARY,
                ids::ADMIN_MAIL_REPORTS_MESSAGES,
                ids::ADMIN_MAIL_REPORTS_ALIGNMENT,
                ids::ADMIN_MAIL_REPORTS_DISPOSITIONS,
                ids::ADMIN_MAIL_REPORTS_SOURCE_IP,
                ids::ADMIN_MAIL_REPORTS_SENDER_DOMAIN,
                ids::ADMIN_MAIL_REPORTS_REPORT_VIEW_BUTTON,
            ] {
                assert!(present(&page, id), "{id} missing");
            }
            for absent in [
                ids::ADMIN_MAIL_REPORTS_EMPTY,
                ids::ADMIN_MAIL_REPORTS_TRUNCATED,
                ids::ADMIN_MAIL_REPORTS_RAW,
            ] {
                assert!(!present(&page, absent), "{absent} painted");
            }
            let messages = crate::testid::find_by_test_id(&page, ids::ADMIN_MAIL_REPORTS_MESSAGES)
                .expect("messages");
            assert_eq!(
                crate::automation::find::text_of(&messages),
                t::messages("10", "2", "1")
            );
            let window =
                crate::testid::find_by_test_id(&page, ids::ADMIN_MAIL_REPORTS_WINDOW_SELECT)
                    .expect("window select");
            assert_eq!(crate::automation::find::text_of(&window), "30");
        });
    }

    #[test]
    fn the_empty_truncated_and_raw_paths_paint_their_own_text() {
        crate::testid::run_on_gtk_thread(|| {
            let (page, w) = build();
            render(&w, &snapshot(vec![]), &noop());
            assert!(present(&page, ids::ADMIN_MAIL_REPORTS_EMPTY));

            let mut snap = snapshot(vec![row()]);
            snap.truncated = true;
            snap.opened = Some(DmarcOpenedReport {
                our_domain: "a.test".into(),
                submitter: "Peer <r@peer.test>".into(),
                report_id: "r-1".into(),
                xml: "<feedback/>".into(),
            });
            render(&w, &snap, &noop());
            assert!(present(&page, ids::ADMIN_MAIL_REPORTS_TRUNCATED));
            assert!(!present(&page, ids::ADMIN_MAIL_REPORTS_EMPTY));
            let raw =
                crate::testid::find_by_test_id(&page, ids::ADMIN_MAIL_REPORTS_RAW).expect("raw");
            assert_eq!(crate::automation::find::text_of(&raw), "<feedback/>");
        });
    }
}
