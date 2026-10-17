//! The Events page's **refused-change list** (`goal/ui/events.md` § Refused
//! scheduling changes): someone tried to change an event on this calendar and
//! the shared apply refused them.
//!
//! **Painted only when there is something to report** — an empty list shows no
//! heading and no ids, so the page a user sees every day is unchanged. Every
//! row's wording is shared Rust (`fauna_client_caldav::refused_change_view`,
//! read through `fauna_client_account_runtime::refused_changes::load_views`);
//! this module only lays the four texts out and carries the one control, a
//! dismiss. There is deliberately no "apply anyway" affordance.

use adw::prelude::*;
use fauna_ui_ids as ids;
use std::rc::Rc;

use crate::i18n::strings::events as events_strings;

/// One refused change as the page paints it — the resolved texts of
/// `fauna_client_caldav::RefusedChangeView`.
#[derive(Debug, Clone, PartialEq)]
pub struct RefusedChangeRow {
    /// What `refused-change-dismiss` hands back.
    pub key: String,
    pub title: String,
    pub author: String,
    /// The reason line, with the repeat count already appended when there has
    /// been more than one attempt (ui.yaml gives a row four texts, not five).
    pub reason: String,
    pub time: String,
}

impl RefusedChangeRow {
    /// Resolve a shared view into the texts a GTK label shows.
    pub fn from_view(view: &fauna_client_caldav::RefusedChangeView) -> Self {
        let lookup = crate::i18n::strings::lookup;
        let reason = match &view.attempts {
            Some(attempts) => format!(
                "{} {}",
                view.reason.resolve(lookup),
                attempts.resolve(lookup)
            ),
            None => view.reason.resolve(lookup),
        };
        Self {
            key: view.key.clone(),
            title: view.headline.resolve(lookup),
            author: view.author.resolve(lookup),
            reason,
            time: fauna_core::format::format_unix_local_date(view.last_refused_at.max(0)),
        }
    }
}

/// The (initially hidden) container for the list; filled by [`refresh`].
pub fn build_refused_changes_panel() -> gtk::Box {
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 4);
    panel.set_margin_start(12);
    panel.set_margin_end(12);
    panel.set_margin_bottom(8);
    panel.set_visible(false);
    panel
}

/// Repaint the panel from `rows`. An empty list hides the panel and removes
/// every id; `on_dismiss` receives a row's key.
pub fn refresh(panel: &gtk::Box, rows: &[RefusedChangeRow], on_dismiss: Rc<dyn Fn(String)>) {
    while let Some(child) = panel.first_child() {
        panel.remove(&child);
    }
    if rows.is_empty() {
        panel.set_visible(false);
        return;
    }
    panel.set_visible(true);

    let list = gtk::Box::new(gtk::Orientation::Vertical, 4);
    crate::testid::set_test_id(&list, ids::REFUSED_CHANGE_LIST);

    let heading = gtk::Label::new(Some(events_strings::refused_changes::TITLE));
    heading.set_halign(gtk::Align::Start);
    heading.add_css_class("heading");
    list.append(&heading);

    for row in rows {
        list.append(&build_row(row, &on_dismiss));
    }
    panel.append(&list);
}

fn build_row(row: &RefusedChangeRow, on_dismiss: &Rc<dyn Fn(String)>) -> gtk::Box {
    let item = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    item.add_css_class("card");
    crate::testid::set_test_id(&item, ids::REFUSED_CHANGE_ITEM);

    let texts = gtk::Box::new(gtk::Orientation::Vertical, 2);
    texts.set_hexpand(true);
    texts.set_margin_start(8);
    texts.set_margin_top(6);
    texts.set_margin_bottom(6);
    for (text, id, css) in [
        (&row.title, ids::REFUSED_CHANGE_TITLE, "heading"),
        (&row.author, ids::REFUSED_CHANGE_AUTHOR, "caption"),
        (&row.reason, ids::REFUSED_CHANGE_REASON, "caption"),
        (&row.time, ids::REFUSED_CHANGE_TIME, "dim-label"),
    ] {
        let label = gtk::Label::new(Some(text));
        label.set_halign(gtk::Align::Start);
        label.set_wrap(true);
        label.set_xalign(0.0);
        label.add_css_class(css);
        crate::testid::set_test_id(&label, id);
        texts.append(&label);
    }
    item.append(&texts);

    // The row's only control — a local store write, so it needs no nest.
    let dismiss = gtk::Button::with_label(events_strings::refused_changes::DISMISS);
    dismiss.set_valign(gtk::Align::Center);
    dismiss.set_margin_end(8);
    crate::testid::set_test_id(&dismiss, ids::REFUSED_CHANGE_DISMISS);
    {
        let key = row.key.clone();
        let on_dismiss = Rc::clone(on_dismiss);
        dismiss.connect_clicked(move |_| on_dismiss(key.clone()));
    }
    item.append(&dismiss);
    item
}
