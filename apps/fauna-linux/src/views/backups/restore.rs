//! Backups restore surfaces — read side (restore history + divergence
//! banner/modal) and the local restore action (snapshot picker + friction
//! bar). Spec: `docs/goal/ui/backups.md` §§ Restore history / Restore
//! divergence / Restore from backup destination.
//!
//! All WS-RPC calls go through the shared `fauna-client-snapshots`
//! crate (priority #1/#2) — the per-app glue here is purely render
//! rules + the friction-bar enable logic.
//!
//! AT-SPI discoverability follows the proven `post-card` row pattern
//! (`views/feed/post_list.rs`): a row carries its searchable text on the
//! AT-SPI accessible `Label` (so the bridge's `do_get_text` returns it
//! via the `get_name()` fallback) while keeping its test id on the
//! accessible `Description` (so `_find_by_test_id` resolves it). The
//! divergence banner is a child *inside* the history-item row so the
//! scoped query `restore-history-item[i]` → `restore-divergence-banner`
//! resolves.

use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;

use crate::client::FaunaClient;
use crate::i18n::strings::backups as s;
use crate::testid::set_test_id;

use fauna_client_snapshots::filesync::{
    RestoreDivergenceRow, RestoreHistoryRow, SnapshotSummaryRow,
};

/// Per-snapshot divergence rows the modal reads, keyed by `snapshot_id`.
/// Populated as each row's `list_restore_divergence` round-trip lands.
pub type DivergenceStore = Rc<RefCell<HashMap<i64, Vec<RestoreDivergenceRow>>>>;

/// Live widgets the message loop pokes when restore data arrives.
pub struct RestoreHandles {
    /// Container box for the history rows (each child is a
    /// `restore-history-item`).
    pub history_list: gtk::Box,
    /// The local-snapshot picker dropdown (`restore-snapshot-select`).
    pub snapshot_select: gtk::DropDown,
    /// Stringified ids of the snapshots currently in the picker, indexed
    /// 1:1 with the dropdown model. The selected id is the match target.
    pub snapshot_ids: Rc<RefCell<Vec<String>>>,
    /// Per-snapshot divergence rows for the modal.
    pub divergence: DivergenceStore,
    /// `restore-progress` — the three-state restore report (idle / running /
    /// done). The click arms `Running`; only the message loop knows the restore
    /// finished, so the terminal state is written from there
    /// (`../../app.rs`'s `message_kind_restored` arms).
    pub progress: gtk::Label,
    /// `restore-warning` — the completed-with-caveat advisory. Hidden until a
    /// restore's reply says `config_present == false` (written, like the DONE
    /// state, from `../../app.rs`'s `MessageKindRestored` arm); the click and a
    /// fresh arrival hide it again, since it reports one restore only.
    pub warning: gtk::Label,
}

/// Build the restore section: the local-restore action card + the
/// collapsible restore-history section. Returns the outer box and the
/// live handles.
pub fn build_restore_section(client: &Rc<FaunaClient>) -> (gtk::Box, RestoreHandles) {
    let outer = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();

    // -- Local restore action ------------------------------------------------
    let (action_box, snapshot_select, snapshot_ids, progress, warning) =
        build_local_restore_action(client);
    outer.append(&action_box);

    // -- Restore history (collapsible) ---------------------------------------
    // A plain Box with role Group (not adw::PreferencesGroup) — Generic-role
    // containers are omitted from the Linux AT-SPI tree under some
    // compositors, so the section must carry an explicit Group role to be
    // discoverable (a known Linux AT-SPI Box-discoverability quirk, tracked
    // internally).
    let history_section = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&history_section, ids::RESTORE_HISTORY_SECTION);

    let section_title = gtk::Label::new(Some(s::RESTORE_SECTION_TITLE));
    section_title.set_halign(gtk::Align::Start);
    section_title.add_css_class("title-4");
    history_section.append(&section_title);

    let history_list = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&history_list, ids::RESTORE_HISTORY_LIST);
    history_section.append(&history_list);

    outer.append(&history_section);

    let handles = RestoreHandles {
        history_list,
        snapshot_select,
        snapshot_ids,
        divergence: Rc::new(RefCell::new(HashMap::new())),
        progress,
        warning,
    };

    (outer, handles)
}

/// Build the local-restore action card: snapshot picker + per-kind
/// checkboxes + the re-type friction bar + progress label.
fn build_local_restore_action(
    client: &Rc<FaunaClient>,
) -> (
    gtk::Box,
    gtk::DropDown,
    Rc<RefCell<Vec<String>>>,
    gtk::Label,
    gtk::Label,
) {
    let group = adw::PreferencesGroup::builder()
        .title(s::RESTORE_LOCAL_TITLE)
        .build();

    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();

    // Snapshot picker — empty until `fetch_message_kind_snapshots` lands.
    let snapshot_select = gtk::DropDown::from_strings(&[]);
    set_test_id(&snapshot_select, ids::RESTORE_SNAPSHOT_SELECT);
    card.append(&snapshot_select);

    // Kinds checkboxes — mail + calendar, both checked by default.
    let kinds_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&kinds_box, ids::RESTORE_KINDS_CHECKBOXES);

    let mail_check = gtk::CheckButton::with_label(s::RESTORE_KINDS_MAIL);
    mail_check.set_active(true);
    set_test_id(&mail_check, ids::RESTORE_KIND_CHECKBOX);
    kinds_box.append(&mail_check);

    let calendar_check = gtk::CheckButton::with_label(s::RESTORE_KINDS_CALENDAR);
    calendar_check.set_active(true);
    set_test_id(&calendar_check, ids::RESTORE_KIND_CHECKBOX);
    kinds_box.append(&calendar_check);

    card.append(&kinds_box);

    // Friction bar — re-type the selected snapshot id.
    let confirm_input = gtk::Entry::new();
    confirm_input.set_placeholder_text(Some(s::RESTORE_CONFIRM_PLACEHOLDER));
    set_test_id(&confirm_input, ids::RESTORE_CONFIRM_INPUT);
    card.append(&confirm_input);

    let confirm_button = gtk::Button::with_label(s::RESTORE_CONFIRM_BUTTON);
    confirm_button.add_css_class("suggested-action");
    confirm_button.set_sensitive(false);
    set_test_id(&confirm_button, ids::RESTORE_CONFIRM_BUTTON);
    crate::offline_gate::declare_wire_kind(
        &confirm_button,
        "fauna.filesync.snapshot.restore_message_kind",
    );
    card.append(&confirm_button);

    // Progress label.
    let progress = gtk::Label::new(Some(s::RESTORE_PROGRESS_IDLE));
    progress.set_halign(gtk::Align::Start);
    progress.add_css_class("dim-label");
    progress.add_css_class("caption");
    set_test_id(&progress, ids::RESTORE_PROGRESS);
    // Keep the test id resolvable on the accessible name even as the
    // visible text changes (GtkLabel derives its name from text).
    progress.update_property(&[gtk::accessible::Property::Label("restore-progress")]);
    card.append(&progress);

    // Completed-with-caveat advisory — its own id, never `error-message`: the
    // restore succeeded. Absent (hidden) until a reply says the account's
    // configuration was not part of it.
    let warning = gtk::Label::new(Some(s::RESTORE_WARNING_CONFIG_ABSENT));
    warning.set_halign(gtk::Align::Start);
    warning.set_wrap(true);
    warning.add_css_class("warning");
    warning.add_css_class("caption");
    set_test_id(&warning, ids::RESTORE_WARNING);
    warning.set_visible(false);
    card.append(&warning);

    group.add(&card);

    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    outer.append(&group);

    // Stringified snapshot ids, parallel to the dropdown model.
    let snapshot_ids: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));

    // -- Friction-bar enable logic -------------------------------------------
    // The button is sensitive iff the typed text exactly equals the
    // SELECTED snapshot's id.
    let recompute_enabled = {
        let confirm_input = confirm_input.clone();
        let confirm_button = confirm_button.clone();
        let snapshot_select = snapshot_select.clone();
        let snapshot_ids = Rc::clone(&snapshot_ids);
        move || {
            let typed = confirm_input.text().to_string();
            let selected = snapshot_select.selected() as usize;
            let ids = snapshot_ids.borrow();
            let matches = ids.get(selected).map(|id| *id == typed).unwrap_or(false);
            confirm_button.set_sensitive(matches && !typed.is_empty());
        }
    };

    {
        let recompute = recompute_enabled.clone();
        confirm_input.connect_changed(move |_| recompute());
    }
    {
        let recompute = recompute_enabled.clone();
        snapshot_select.connect_selected_notify(move |_| recompute());
    }

    // -- Restore dispatch ----------------------------------------------------
    {
        let c = Rc::clone(client);
        let snapshot_select = snapshot_select.clone();
        let snapshot_ids = Rc::clone(&snapshot_ids);
        let confirm_input = confirm_input.clone();
        let progress = progress.clone();
        let warning = warning.clone();
        confirm_button.connect_clicked(move |_| {
            let selected = snapshot_select.selected() as usize;
            let ids = snapshot_ids.borrow();
            let Some(id_str) = ids.get(selected) else {
                return;
            };
            let Ok(snapshot_id) = id_str.parse::<i64>() else {
                return;
            };
            let confirm_id = confirm_input.text().to_string();
            progress.set_text(s::RESTORE_PROGRESS_RUNNING);
            warning.set_visible(false);
            c.restore_message_kind(snapshot_id, confirm_id);
        });
    }

    (outer, snapshot_select, snapshot_ids, progress, warning)
}

/// Populate the local-snapshot picker from `fauna.filesync.snapshot.list`.
/// With one snapshot the dropdown auto-selects it (index 0).
pub fn populate_snapshot_select(handles: &RestoreHandles, snapshots: &[SnapshotSummaryRow]) {
    let ids: Vec<String> = snapshots.iter().map(|r| r.id.to_string()).collect();
    let labels: Vec<String> = snapshots
        .iter()
        .map(|r| {
            fauna_client_snapshots::snapshot_restore_option_label(r.message_kind.as_deref(), r.id)
        })
        .collect();
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();

    let model = gtk::StringList::new(&label_refs);
    // The ids BEFORE the model: `set_model` re-fires the selection notify, and
    // the friction bar's recompute reads `snapshot_ids` — assigned after, it
    // compared the typed id against the previous list (empty on the first
    // paint), so an id typed before the list landed stayed greyed for good.
    // The closing notify covers a model swap that leaves the selection where
    // it was, which fires no notify at all.
    *handles.snapshot_ids.borrow_mut() = ids;
    handles.snapshot_select.set_model(Some(&model));
    if !snapshots.is_empty() {
        handles.snapshot_select.set_selected(0);
    }
    handles.snapshot_select.notify("selected");
}

/// Rebuild the restore-history list from the freshly-fetched rows. Clears
/// the per-snapshot divergence store; the message loop then re-fires one
/// `fetch_restore_divergence` per row to repopulate the banners.
pub fn populate_restore_history(
    handles: &RestoreHandles,
    rows: &[RestoreHistoryRow],
    client: &Rc<FaunaClient>,
) {
    // Clear existing rows.
    while let Some(child) = handles.history_list.first_child() {
        handles.history_list.remove(&child);
    }
    handles.divergence.borrow_mut().clear();

    for row in rows {
        let row_widget = build_history_row(row, &handles.divergence, client);
        handles.history_list.append(&row_widget);
    }
}

/// Build one restore-history row. Carries its text on the AT-SPI Label and
/// its test id on the Description (proven `post-card` pattern), with the
/// divergence banner as a hidden child until divergence rows arrive.
fn build_history_row(
    row: &RestoreHistoryRow,
    divergence: &DivergenceStore,
    client: &Rc<FaunaClient>,
) -> gtk::Box {
    let source = match &row.source_member_id {
        // `None` → local snapshot (always today; Plan 4 populates the
        // destination provenance).
        None => s::RESTORE_SOURCE_LOCAL.to_string(),
        Some(member) => fauna_core::format::hex_short(member),
    };
    let when = crate::client::format_epoch_us(row.completed_at.saturating_mul(1_000_000));
    let text = s::restore_history_row(&row.kinds_restored, &source, &when);

    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(8)
        .margin_end(8)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    // Description = test id (findable); Label = row text (get_text returns it).
    card.update_property(&[
        gtk::accessible::Property::Description("restore-history-item"),
        gtk::accessible::Property::Label(text.as_str()),
    ]);
    card.set_widget_name("restore-history-item");
    card.set_tooltip_text(Some(&text));

    // Hidden marker carrying the snapshot id so `populate_divergence` can
    // match this card when the per-row divergence round-trip lands. A 1px
    // label (not 0px — AT-SPI needs nonzero allocation, but this isn't
    // test-queried so size is irrelevant; kept minimal + invisible).
    let marker = gtk::Label::new(None);
    marker.set_widget_name(&format!("snapshot:{}", row.snapshot_id));
    marker.set_visible(false);
    card.append(&marker);

    // Visible text label (so a human sees the row too).
    let label = gtk::Label::new(Some(&text));
    label.set_halign(gtk::Align::Start);
    label.set_wrap(true);
    card.append(&label);

    // Divergence banner — a Button (clickable via AT-SPI do_action) hidden
    // until `populate_divergence` reveals it. Lives INSIDE the row so the
    // scoped query `restore-history-item[i]` → `restore-divergence-banner`
    // resolves.
    let banner = gtk::Button::new();
    banner.add_css_class("flat");
    banner.add_css_class("warning");
    set_test_id(&banner, ids::RESTORE_DIVERGENCE_BANNER);
    banner.set_visible(false);
    card.append(&banner);

    // Clicking the banner opens the forensic details modal.
    {
        let divergence = Rc::clone(divergence);
        let snapshot_id = row.snapshot_id;
        let _client = Rc::clone(client);
        banner.connect_clicked(move |btn| {
            let rows = divergence
                .borrow()
                .get(&snapshot_id)
                .cloned()
                .unwrap_or_default();
            let parent = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
            present_divergence_modal(parent.as_ref(), &rows);
        });
    }

    card
}

/// Update one history row's divergence banner once its
/// `list_restore_divergence` round-trip lands. Stores the rows for the
/// modal and reveals/labels the banner when ≥1 row exists.
pub fn populate_divergence(
    handles: &RestoreHandles,
    snapshot_id: i64,
    rows: &[RestoreDivergenceRow],
) {
    handles
        .divergence
        .borrow_mut()
        .insert(snapshot_id, rows.to_vec());

    if rows.is_empty() {
        return;
    }

    // Reveal the matching row's banner. Each history card carries a hidden
    // `snapshot:<id>` marker child (see `build_history_row`); walk the list,
    // match by that id, and reveal + label the card's banner button.
    let banner_text = s::restore_divergence_banner(&rows.len().to_string());

    let mut child = handles.history_list.first_child();
    while let Some(card) = child {
        if let Some(card_box) = card.downcast_ref::<gtk::Box>()
            && card_snapshot_id(card_box) == Some(snapshot_id)
            && let Some(banner) = find_banner(card_box)
        {
            banner.set_label(&banner_text);
            banner.set_visible(true);
        }
        child = card.next_sibling();
    }
}

/// Present the forensic divergence-details modal. No action buttons — the
/// modal is read-only/forensic (close only). Lists one
/// `restore-divergence-details-item` per row + the lost-writes footer.
fn present_divergence_modal(parent: Option<&gtk::Window>, rows: &[RestoreDivergenceRow]) {
    let dialog = adw::Window::builder()
        .title(s::RESTORE_DIVERGENCE_MODAL_TITLE)
        .modal(true)
        .default_width(520)
        .default_height(360)
        .build();
    if let Some(win) = parent {
        dialog.set_transient_for(Some(win));
    }
    set_test_id(&dialog, ids::RESTORE_DIVERGENCE_DETAILS_MODAL);

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_top(16)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .build();

    let title = gtk::Label::new(Some(s::RESTORE_DIVERGENCE_MODAL_TITLE));
    title.set_halign(gtk::Align::Start);
    title.add_css_class("title-2");
    content.append(&title);

    for row in rows {
        let mua = row
            .mua_id
            .clone()
            .unwrap_or_else(|| s::RESTORE_DIVERGENCE_UNKNOWN_MUA.to_string());
        let text = s::restore_divergence_detail_row(
            &row.collection,
            &mua,
            &row.client_modseq.to_string(),
            &row.server_modseq.to_string(),
            &row.lost_event_count.to_string(),
        );

        let item = gtk::Label::new(Some(&text));
        item.set_halign(gtk::Align::Start);
        item.set_wrap(true);
        item.set_selectable(true);
        // Description = test id; Label = row text (get_text returns it).
        item.update_property(&[
            gtk::accessible::Property::Description("restore-divergence-details-item"),
            gtk::accessible::Property::Label(text.as_str()),
        ]);
        item.set_widget_name("restore-divergence-details-item");
        content.append(&item);
    }

    let footer = gtk::Label::new(Some(s::RESTORE_DIVERGENCE_FOOTER));
    footer.set_halign(gtk::Align::Start);
    footer.set_wrap(true);
    footer.add_css_class("dim-label");
    footer.add_css_class("caption");
    content.append(&footer);

    // Close only — no action buttons (forensic).
    let close_btn = gtk::Button::with_label(s::RESTORE_DIVERGENCE_CLOSE);
    close_btn.set_halign(gtk::Align::End);
    {
        let d = dialog.clone();
        close_btn.connect_clicked(move |_| d.close());
    }
    content.append(&close_btn);

    dialog.set_content(Some(&content));
    dialog.present();
}

// --- helpers ----------------------------------------------------------------

/// Read a history card's snapshot id from its hidden `snapshot:<id>` marker
/// child (the card's own widget name carries the test id, so the snapshot id
/// rides on a dedicated marker label instead). Used by `populate_divergence`
/// to find the row whose banner to reveal.
fn card_snapshot_id(card: &gtk::Box) -> Option<i64> {
    let mut child = card.first_child();
    while let Some(w) = child {
        let name = w.widget_name();
        if let Some(rest) = name.strip_prefix("snapshot:") {
            return rest.parse::<i64>().ok();
        }
        child = w.next_sibling();
    }
    None
}

fn find_banner(card: &gtk::Box) -> Option<gtk::Button> {
    let mut child = card.first_child();
    while let Some(w) = child {
        if w.widget_name() == "restore-divergence-banner" {
            return w.downcast::<gtk::Button>().ok();
        }
        child = w.next_sibling();
    }
    None
}
