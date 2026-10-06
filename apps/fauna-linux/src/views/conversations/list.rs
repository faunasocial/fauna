//! Conversation list pane: header (heading + new + sort) + search + rows.
//!
//! Renders `ConversationsSnapshot.threads` as `gtk::ListBoxRow` entries;
//! click → `manager.select_thread(id)`. The `conversation-item` test ID
//! sits on a 1px hidden marker child of each row's content (AT-SPI doesn't
//! reliably expose `set_widget_name` on `ListBoxRow` itself).
//!
//! Indexed list IDs are scoped queries; the AT-SPI
//! bridge resolves `conversation-item[i]` by row position.

use fauna_ui_ids as ids;
use std::sync::Arc;

use gtk::prelude::*;

use fauna_conversations::{
    ConversationsManager,
    snapshot::{GuardianState, ThreadSummary, next_sort_order},
};

pub struct ConversationList {
    pub root: gtk::Box,
    list_box: gtk::ListBox,
    /// Held so `render` can wire each row's clickable card to
    /// `manager.select_thread`.
    manager: Arc<ConversationsManager>,
    /// This pane's own session actor (`FaunaClient::actor_id()`), fed to
    /// `crate::backup_audit::observe_thread_activity` — never a fresh
    /// `active_actor_id_hex()` read, which can name a different account on a
    /// bound (secondary) launch (`account-scoping.md:818-837`).
    actor_id_hex: String,
}

impl ConversationList {
    pub fn new(manager: Arc<ConversationsManager>, actor_id_hex: String) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);

        // Header bar: page-heading | new-conversation-button | conversation-sort.
        let header = adw::HeaderBar::new();
        let title = gtk::Label::new(Some(crate::i18n::strings::conversations::list::TITLE));
        crate::testid::set_test_id(&title, ids::PAGE_HEADING);
        header.set_title_widget(Some(&title));

        let new_btn = gtk::Button::from_icon_name("mail-message-new-symbolic");
        new_btn.add_css_class("flat");
        new_btn.set_tooltip_text(Some(
            crate::i18n::strings::conversations::list::NEW_CONVERSATION,
        ));
        crate::testid::set_test_id(&new_btn, ids::NEW_CONVERSATION_BUTTON);
        {
            let m = manager.clone();
            new_btn.connect_clicked(move |_| {
                m.start_new_conversation();
            });
        }
        header.pack_end(&new_btn);

        let sort_btn = gtk::Button::from_icon_name("view-sort-descending-symbolic");
        sort_btn.add_css_class("flat");
        sort_btn.set_tooltip_text(Some(crate::i18n::strings::common::SORT));
        crate::testid::set_test_id(&sort_btn, ids::CONVERSATION_SORT);
        // Wire the button to the shared cycle: `next_sort_order` owns the
        // arity (latest → oldest → unread → back), the manager reorders and
        // notifies, so the observer re-renders the sorted list. Reading the
        // order back off `snapshot()` keeps the tap acting on live state.
        {
            let m = manager.clone();
            sort_btn.connect_clicked(move |_| {
                m.set_sort(next_sort_order(m.snapshot().sort));
            });
        }
        header.pack_end(&sort_btn);

        root.append(&header);

        // Search entry.
        let search = gtk::SearchEntry::new();
        search.set_placeholder_text(Some(
            crate::i18n::strings::conversations::list::SEARCH_PLACEHOLDER,
        ));
        search.set_margin_start(8);
        search.set_margin_end(8);
        search.set_margin_top(4);
        search.set_margin_bottom(4);
        crate::testid::set_test_id(&search, ids::CONVERSATION_SEARCH_BOX);
        // Wire the box to the shared filter: the manager filters
        // `snapshot().threads` by the query (case-insensitive over label +
        // snippet) and notifies, so the observer re-renders the filtered list.
        // Empty → `None` (no active search), matching web/android
        // (`set_search_query(v || undefined)` / `.ifEmpty { null }`).
        {
            let m = manager.clone();
            search.connect_search_changed(move |entry| {
                let text = entry.text();
                let query = if text.is_empty() {
                    None
                } else {
                    Some(text.to_string())
                };
                m.set_search_query(query);
            });
        }
        root.append(&search);

        // ListBox in a scrolled window.
        let list_box = gtk::ListBox::new();
        list_box.set_selection_mode(gtk::SelectionMode::Single);
        list_box.add_css_class("boxed-list");

        let placeholder = adw::StatusPage::builder()
            .title(crate::i18n::strings::conversations::list::NO_CONVERSATIONS)
            .icon_name("mail-unread-symbolic")
            .build();
        list_box.set_placeholder(Some(&placeholder));

        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .child(&list_box)
            .build();

        root.append(&scrolled);

        Self {
            root,
            list_box,
            manager,
            actor_id_hex,
        }
    }

    pub fn render(&self, threads: &[ThreadSummary], selected: Option<&str>) {
        // Feed the backup audit loop's freshness comparison: the newest activity
        // this client has actually *displayed* is its own, source-untrusted
        // evidence that data this recent exists, and the audit compares that
        // against what each backup destination is holding. Monotonic and a no-op
        // on a repeat render — `crate::backup_audit` owns the contract, and
        // `backups.md` § Audit-alert surface the surface it drives. Keyed by
        // THIS pane's own session actor (`self.actor_id_hex`), never a fresh
        // `active_actor_id_hex()` read (`account-scoping.md:818-837`).
        if let Some(newest) = threads.iter().map(|t| t.last_activity_ms).max() {
            crate::backup_audit::observe_thread_activity(&self.actor_id_hex, newest);
        }

        // Clear existing rows.
        while let Some(child) = self.list_box.first_child() {
            self.list_box.remove(&child);
        }

        let mut selected_row: Option<gtk::ListBoxRow> = None;
        for t in threads {
            let row = build_row(t, &self.manager);
            self.list_box.append(&row);
            if Some(t.thread_id.0.as_str()) == selected {
                selected_row = Some(row);
            }
        }
        if let Some(row) = selected_row {
            self.list_box.select_row(Some(&row));
        }
    }
}

fn build_row(t: &ThreadSummary, manager: &Arc<ConversationsManager>) -> gtk::ListBoxRow {
    // Top: label | protocol-icon | timestamp. Shared empty-label fallback
    // (`fauna_core::format::thread_label_display`): a blank label renders the
    // canonical `(no subject)` placeholder instead of an empty heading.
    let label_text =
        fauna_core::format::thread_label_display(&t.label).resolve(crate::i18n::strings::lookup);
    let handle_label = gtk::Label::new(Some(&label_text));
    handle_label.set_halign(gtk::Align::Start);
    handle_label.set_hexpand(true);
    handle_label.add_css_class("heading");
    handle_label.set_ellipsize(gtk::pango::EllipsizeMode::End);

    // The glyph is the snapshot's — on a bridged room the one its bridge
    // declared, with the bridge's declared label beside it so two bridges
    // sharing a glyph still read apart (`conversations.md` § Where logic lives
    // → *The `Bridged` adapter*, ruling 2 (a)). No app-side mapping.
    let glyph = crate::source_glyph::source_glyph_emoji(t.glyph);
    let protocol_icon = match &t.bridge {
        Some(bridge) => gtk::Label::new(Some(&format!("{glyph} {}", bridge.label))),
        None => gtk::Label::new(Some(glyph)),
    };
    protocol_icon.add_css_class("dim-label");
    crate::testid::set_test_id(&protocol_icon, ids::PROTOCOL_ICON);
    if let Some(bridge) = &t.bridge {
        crate::testid::set_test_attr(&protocol_icon, "bridge", &bridge.id);
    }

    let time_text = format_time_brief(t.last_activity_ms);
    let time_label = gtk::Label::new(Some(&time_text));
    time_label.add_css_class("fauna-muted");
    time_label.add_css_class("caption");
    time_label.set_halign(gtk::Align::End);
    // `conversation-item-timestamp`, read scoped within this row's
    // `conversation-item` card; absent (untagged) when the thread carries no
    // activity time, as on tui.
    if !time_text.is_empty() {
        crate::testid::set_test_id(&time_label, ids::CONVERSATION_ITEM_TIMESTAMP);
    }

    let top_line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    top_line.append(&handle_label);
    top_line.append(&protocol_icon);
    // The family gate's marker, only while the nest reports one for this
    // room's peer (`family-safety.md` § The bridge-DM gate → *App
    // affordance*): computed there, painted here, and the row still opens.
    if let Some(state) = t.guardian_state {
        top_line.append(&guardian_state_label(state));
    }
    top_line.append(&time_label);

    // Bottom: snippet | unread-badge.
    let subject_label = gtk::Label::new(Some(&t.snippet));
    subject_label.set_halign(gtk::Align::Start);
    subject_label.set_hexpand(true);
    subject_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    subject_label.add_css_class("fauna-muted");
    crate::testid::set_test_id(&subject_label, ids::DM_SUBJECT);

    let bottom_line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    bottom_line.append(&subject_label);
    if t.unread_count > 0 {
        let badge = gtk::Label::new(Some(&t.unread_count.to_string()));
        badge.add_css_class("fauna-accent");
        badge.set_halign(gtk::Align::End);
        crate::testid::set_test_id(&badge, ids::DM_UNREAD_INDICATOR);
        bottom_line.append(&badge);
    }

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 4);
    vbox.set_margin_top(8);
    vbox.set_margin_bottom(8);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);
    vbox.append(&top_line);
    vbox.append(&bottom_line);

    // The whole row content is a flat `gtk::Button` carrying the
    // `conversation-item` ID. A Button (not a Label marker) is what makes the
    // row actuable coordinate-free: the e2e AT-SPI bridge calls `do_action(0)`
    // on the element holding the test id, which only fires `clicked` on a
    // widget that exposes an AT-SPI action — GTK4 does not surface one on a
    // bare `GtkListBoxRow`, and a Label has none. Same pattern as the events
    // agenda card (views/events/event_list.rs). Click → `select_thread`; the
    // resulting snapshot tick drives `render`, which re-selects the row for the
    // visual highlight.
    let card = gtk::Button::new();
    card.add_css_class("flat");
    card.set_hexpand(true);
    card.set_child(Some(&vbox));
    crate::testid::set_test_id(&card, ids::CONVERSATION_ITEM);
    // A child-bearing Button has no `label()`, so without this every row reads
    // as "" and a failed click cannot say which row it addressed (convention 6).
    crate::testid::set_test_text(&card, &label_text);
    {
        let m = manager.clone();
        let tid = t.thread_id.0.clone();
        card.connect_clicked(move |_| {
            m.select_thread(fauna_conversations::ThreadId(tid.clone()));
        });
    }

    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&card));
    row.set_widget_name(&t.thread_id.0);
    // Selection (highlight) is snapshot-driven via `render`; the inner card
    // Button owns the click, so the row itself need not be activatable.
    row.set_activatable(false);
    row
}

/// `conversation-guardian-state` — the family gate's marker, the same widget
/// on the list row and in the thread header: the shared localized text, the
/// wire word on its `state` attribute.
pub(crate) fn guardian_state_label(state: GuardianState) -> gtk::Label {
    let marker = gtk::Label::new(Some(state.label()));
    marker.add_css_class("warning");
    marker.add_css_class("caption");
    crate::testid::set_test_id(&marker, ids::CONVERSATION_GUARDIAN_STATE);
    crate::testid::set_test_attr(&marker, "state", state.attr_token());
    marker
}

/// A thread's last-activity time for the list row: the shared contextual
/// formatter (today → local clock, Yesterday / a weekday, else a short date),
/// resolved in the user's local timezone. Replaces the old hand-rolled
/// `{h}:{m}` that rendered UTC, not local time. See
/// `docs/goal/behavior/value-formatting.md` § Conversation timestamp.
fn format_time_brief(ms: i64) -> String {
    if ms == 0 {
        return String::new();
    }
    let now_ms = fauna_core::data::Timestamp::now_millis_or_zero() as i64;
    crate::i18n::conversation_timestamp(ms, now_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_backup::audit::AuditStateStore;
    use fauna_conversations::address::Rail;
    use fauna_conversations::thread::ThreadFlavor;

    fn thread(last_activity_ms: i64) -> ThreadSummary {
        ThreadSummary {
            thread_id: fauna_conversations::thread::ThreadId("t-1".into()),
            rail: Rail::FaunaMls,
            glyph: Rail::FaunaMls.glyph(),
            flavor: ThreadFlavor::OneToOne,
            label: "t".into(),
            snippet: String::new(),
            last_activity_ms,
            unread_count: 0,
            participant_count: 1,
            bridge: None,
            guardian_state: None,
        }
    }

    /// The render-path observation must land in THIS pane's own actor's audit
    /// store — the one the pane was constructed with — never a bare
    /// `active_actor_id_hex()` read, which the fixed call site no longer
    /// performs at all (`account-scoping.md:818-837`; sibling of tui's
    /// `backup_audit_observation_keys_by_the_session_account_not_active`).
    /// Reds on a regression back to a parameterless `observe_thread_activity()`.
    #[test]
    fn render_observes_into_the_panes_own_actor() {
        crate::testid::run_on_gtk_thread(|| {
            // Test-only, never a real device's account — safe to touch its
            // scoped state dir and clean it up below.
            let actor =
                "ee55ee55ee55ee55ee55ee55ee55ee55ee55ee55ee55ee55ee55ee55ee55ee55".to_string();
            let manager = ConversationsManager::new();
            let list = ConversationList::new(manager, actor.clone());
            list.render(&[thread(1_700_000_000_000)], None);

            let snapshot = crate::backup_audit::store(&actor)
                .load()
                .expect("a fresh actor's store degrades to its default, never an error");
            assert_eq!(
                snapshot.observed_high_water,
                Some(1_700_000_000),
                "the observation must land in the pane's OWN actor's store"
            );

            let _ = std::fs::remove_dir_all(crate::sync::backup_state_dir(&actor));
        });
    }

    /// The `conversation-item` card reads as its thread's title, not "": the
    /// card is a child-bearing Button with no `label()`, so only a declared
    /// test text gives `get_texts("conversation-item")` — and the failed-click
    /// diagnosis built on it — something to report (convention 6).
    #[test]
    fn a_conversation_item_card_reads_as_its_thread_title() {
        crate::testid::run_on_gtk_thread(|| {
            let mut t = thread(1_700_000_000_000);
            t.label = "Q4 budget divider-change".into();
            let row = build_row(&t, &ConversationsManager::new());
            let card = crate::testid::find_by_test_id(&row, ids::CONVERSATION_ITEM)
                .expect("the row carries its conversation-item card");
            assert_eq!(
                crate::automation::find::text_of(&card),
                "Q4 budget divider-change"
            );
        });
    }
}
