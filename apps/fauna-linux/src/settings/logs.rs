//! The Settings → Logs sub-page — the client's durable, in-app log record.
//!
//! Target state: `docs/goal/architecture/apps/observability.md` § Surfaces.
//! Renders the **process-global** `fauna_log` ring (`snapshot()`) newest-first
//! with a severity filter, a copy-to-clipboard affordance, and a clear button.
//! Unlike the other settings sub-pages this one needs **no** client handle — the
//! ring is a process global filled by every `tracing` event the client emits, so
//! the page self-wires (priority #2: all log capture lives in shared `fauna-log`,
//! this layer is dumb rendering).
//!
//! The row/list rendering (and the filter↔level mapping) lives in the shared
//! `crate::logs_view` module, which the **admin Logs view** (`views/admin.rs`,
//! the nest's ring over `fauna.admin.logs`) renders with too — so the two
//! surfaces stay byte-identical (priority #2/#3). This page differs only in its
//! *source*: it filters by re-reading the local ring (`snapshot_at_least`); the
//! admin view filters a fetched `Vec` in memory.
//!
//! **Redaction rule** (observability.md): we render whatever `tracing` captured,
//! and call sites are forbidden from logging message plaintext or secrets — those
//! would land here *and* in the on-disk rolling file. This page enforces nothing;
//! the rule is upheld at the call sites the reviewer guards.

use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

use fauna_log::LogEntry;

use crate::i18n::strings::logs as S;
use crate::logs_view;
use crate::testid::set_test_id;

/// Widget handles the render closures need.
struct Widgets {
    filter: gtk::DropDown,
    list_group: adw::PreferencesGroup,
    placeholder_row: adw::ActionRow,
    rows: RefCell<Vec<adw::ActionRow>>,
}

/// `Rc`-shared into the page's closures (map / filter-change / copy / clear).
struct Ctx {
    w: Widgets,
}

/// Build the "Logs" preferences page (the client's in-app log record).
pub fn build_logs_page() -> gtk::Box {
    let page = adw::PreferencesPage::builder()
        .title(S::TITLE)
        .icon_name("utilities-system-monitor-symbolic")
        .build();

    // --- Top group: heading + landmark + error + controls (filter, copy, clear) ---
    let top_group = adw::PreferencesGroup::builder()
        .title(S::TITLE)
        .description(S::DESCRIPTION)
        .build();

    // error-message — every page has one (Rule 2), hidden until set. The Logs
    // page reads a process global, so it rarely errors, but the element is
    // present for uniformity with the other settings sub-pages.
    let error_label = gtk::Label::builder().visible(false).build();
    error_label.add_css_class("error");
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    top_group.add(&error_label);

    // Severity filter (DropDown → snapshot_at_least).
    let filter = gtk::DropDown::from_strings(&logs_view::filter_labels());
    filter.set_valign(gtk::Align::Center);
    set_test_id(&filter, ids::LOG_LEVEL_FILTER);
    let filter_row = adw::ActionRow::builder()
        .title(S::FILTER_LABEL)
        .activatable(false)
        .build();
    filter_row.add_suffix(&filter);
    top_group.add(&filter_row);

    // Copy (to clipboard) + Clear (the ring) actions.
    let copy_button = gtk::Button::builder()
        .label(S::COPY_BUTTON)
        .valign(gtk::Align::Center)
        .build();
    set_test_id(&copy_button, ids::LOG_COPY_BUTTON);
    let clear_button = gtk::Button::builder()
        .label(S::CLEAR_BUTTON)
        .css_classes(["destructive-action"])
        .valign(gtk::Align::Center)
        .build();
    set_test_id(&clear_button, ids::LOG_CLEAR_BUTTON);
    let actions_row = adw::ActionRow::builder().activatable(false).build();
    actions_row.add_suffix(&copy_button);
    actions_row.add_suffix(&clear_button);
    top_group.add(&actions_row);

    page.add(&top_group);

    // --- List group: placeholder + indexed log-entry rows (newest first) ---
    let list_group = adw::PreferencesGroup::new();
    let placeholder_row = adw::ActionRow::builder().title(S::EMPTY).build();
    list_group.add(&placeholder_row);
    page.add(&list_group);

    let ctx = Rc::new(Ctx {
        w: Widgets {
            filter: filter.clone(),
            list_group,
            placeholder_row,
            rows: RefCell::new(Vec::new()),
        },
    });

    // Initial render from the build-time snapshot.
    render(&ctx);

    // Re-render whenever the page is shown (navigated to) so events captured
    // since build-time appear. The settings sub-stack maps the child on switch.
    {
        let ctx = Rc::clone(&ctx);
        page.connect_map(move |_| render(&ctx));
    }
    // Filter change → re-render at the new threshold.
    {
        let ctx = Rc::clone(&ctx);
        filter.connect_selected_notify(move |_| render(&ctx));
    }
    // Copy → put the currently-rendered lines on the clipboard.
    {
        let ctx = Rc::clone(&ctx);
        copy_button.connect_clicked(move |btn| {
            btn.clipboard().set_text(&fauna_log::format::rendered_text(
                &filtered_entries(&ctx),
                logs_view::local_offset_secs(),
            ));
        });
    }
    // Clear → drop the in-memory ring (NOT the on-disk file) + re-render empty.
    {
        let ctx = Rc::clone(&ctx);
        clear_button.connect_clicked(move |_| {
            fauna_log::clear();
            render(&ctx);
        });
    }

    crate::testid::wrap_page_with_heading(S::TITLE, ids::SETTINGS_LOGS, &page)
}

/// The entries the active filter selects, by re-reading the **local** ring
/// (`snapshot_at_least`) — distinct from the admin view, which filters a fetched
/// `Vec` (`fauna_log::format::filter_entries`). Oldest-first as `fauna_log`
/// returns them (`logs_view::populate` reverses for newest-first display).
fn filtered_entries(ctx: &Rc<Ctx>) -> Vec<LogEntry> {
    fauna_log::snapshot_filtered(ctx.w.filter.selected())
}

/// Rebuild the list from the ring at the active filter, newest-first.
fn render(ctx: &Rc<Ctx>) {
    let entries = filtered_entries(ctx);
    logs_view::populate(
        &ctx.w.list_group,
        &ctx.w.placeholder_row,
        &ctx.w.rows,
        &entries,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_builds() {
        crate::testid::run_on_gtk_thread(|| {
            // Builds with every ui.yaml ID present, reading the process-global ring.
            let _page = build_logs_page();
        });
    }
}
