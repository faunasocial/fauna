use crate::client::FaunaClient;
use crate::i18n::strings::common;
use adw::prelude::*;
use fauna_client_notifications::NotificationDestination;
use fauna_core::possibility::{Possibility, ReadState};
use fauna_ui_ids as ids;
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// The shared decision's answer for each painted row, index-for-index with the
/// list box's rows — read at activation time so a click never re-derives it.
pub type Destinations = Rc<RefCell<Vec<Option<NotificationDestination>>>>;

thread_local! {
    /// Session-local set of notification ids whose muted-word collapse the user
    /// revealed (`notifications.md` § Muted words rule 3) — render state, the
    /// twin of the feed's `REVEALED_MUTED_POSTS`. The mute stays.
    static REVEALED_MUTED_NOTIFICATIONS: RefCell<std::collections::HashSet<i64>> =
        RefCell::new(std::collections::HashSet::new());
}

pub struct NotificationHandles {
    pub count_badge: gtk::Label,
    pub list_box: gtk::ListBox,
    pub destinations: Destinations,
    /// The page-wide actions, *Mark all read* and *Clear all*.
    pub actions: PageActions,
}

/// The page-wide actions and what they wait on (`notifications.md` § Errors &
/// edge cases, *The page-wide actions wait on the first read*): both are
/// painted from the shared `fauna_client_notifications::mark_all_read_
/// possibility` / `clear_all_possibility` over the page's read state (and,
/// for *Clear all*, the rows it shows), through [`crate::possible::paint`]
/// (reason on the tooltip), and each press asks the same answer again and
/// refuses with its reason on `error-message` (`ui/README.md` § Offer only
/// what is possible, rules 5 and 6). tui's `mark_read_answer` / `clear_answer`.
#[derive(Clone)]
pub struct PageActions {
    /// `notification-mark-read`.
    pub mark_read_button: gtk::Button,
    /// `notification-clear`.
    pub clear_button: gtk::Button,
    /// Where the page's list read stands: `Pending` until it first answers,
    /// `Failed` after a first read that came back with an error (until a later
    /// one succeeds); a failure after a resolved read keeps it resolved.
    read: Rc<Cell<ReadState>>,
    /// How many rows the page shows (a collapsed muted row counts).
    rows: Rc<Cell<usize>>,
    /// The newest `created_at` the page has painted — the bound a clear-all
    /// sends, so a row that arrived after the user looked survives
    /// (`notifications.md` § Retention rule 2). `None` on an empty page.
    pub clear_up_to: Rc<Cell<Option<i64>>>,
}

impl PageActions {
    fn new(mark_read_button: gtk::Button, clear_button: gtk::Button) -> Self {
        let this = Self {
            mark_read_button,
            clear_button,
            read: Rc::new(Cell::new(ReadState::Pending)),
            rows: Rc::new(Cell::new(0)),
            clear_up_to: Rc::new(Cell::new(None)),
        };
        this.repaint();
        this
    }

    /// *Mark all read*'s answer.
    pub fn mark_read_answer(&self) -> Possibility {
        fauna_client_notifications::mark_all_read_possibility(self.read.get())
    }

    /// *Clear all*'s answer.
    pub fn clear_answer(&self) -> Possibility {
        fauna_client_notifications::clear_all_possibility(self.read.get(), self.rows.get())
    }

    fn repaint(&self) {
        crate::possible::paint(&self.mark_read_button, None, self.mark_read_answer());
        crate::possible::paint(&self.clear_button, None, self.clear_answer());
    }

    /// The page's list read answered with `rows`, the newest at `newest`.
    fn loaded(&self, rows: usize, newest: Option<i64>) {
        self.read.set(ReadState::Resolved);
        self.rows.set(rows);
        self.clear_up_to.set(newest);
        self.repaint();
    }

    /// The page's list read came back with an error. Rows already shown stay
    /// actionable; before any resolved read the actions are blocked with the
    /// failure.
    pub fn read_failed(&self) {
        if self.read.get() != ReadState::Resolved {
            self.read.set(ReadState::Failed);
            self.repaint();
        }
    }

    /// Wire both presses: each asks its answer and refuses onto
    /// `error_label` (the page's `error-message`) before doing anything.
    /// *Mark all read* then runs `on_mark_read`; *Clear all* sends `on_clear`
    /// the bound.
    pub fn connect(
        &self,
        error_label: &gtk::Label,
        on_mark_read: impl Fn() + 'static,
        on_clear: impl Fn(i64) + 'static,
    ) {
        {
            let this = self.clone();
            let error_label = error_label.clone();
            self.mark_read_button.connect_clicked(move |_| {
                if crate::form_gate::refuse(&this.mark_read_answer(), &error_label) {
                    return;
                }
                on_mark_read();
            });
        }
        {
            let this = self.clone();
            let error_label = error_label.clone();
            self.clear_button.connect_clicked(move |_| {
                if crate::form_gate::refuse(&this.clear_answer(), &error_label) {
                    return;
                }
                if let Some(bound) = this.clear_up_to.get() {
                    on_clear(bound);
                }
            });
        }
    }
}

/// Whether a row with this destination is offered as a control. The shared
/// decision's answer minus its one app-local subtraction: `Family` only while
/// the family tab is gated in (tui's `navigable()` shape) — a control into a
/// page the sidebar hides would be a dead end.
fn navigable(dest: &Option<NotificationDestination>, family_gated_in: bool) -> bool {
    match dest {
        None => false,
        Some(NotificationDestination::Family) => family_gated_in,
        Some(_) => true,
    }
}

/// Re-derive every row's `activatable` flag from its stored destination and the
/// current family gate. A row with no destination is plain text, never a
/// control that silently does nothing; rerun when the gate moves, because
/// `fauna.family.status` can land after the first notifications paint.
pub fn sync_activatable(
    list_box: &gtk::ListBox,
    destinations: &Destinations,
    family_gated_in: bool,
) {
    for (i, dest) in destinations.borrow().iter().enumerate() {
        if let Some(row) = list_box.row_at_index(i as i32) {
            row.set_activatable(navigable(dest, family_gated_in));
        }
    }
}

/// The cross-page navigation a `notification-item` activation dispatches into,
/// one per [`NotificationDestination`] arm — each routed into the destination
/// page's own gesture, as a direct click there would (`notifications.md`
/// § Deep-link destinations; mirrors tui's `open_notification`).
pub struct NotificationNav {
    /// The main content stack, switched to the destination page.
    pub stack: gtk::Stack,
    /// Opens the Feed post-detail pane for a post named only by id — the same
    /// door a `search-result-item` uses; it also resolves a post the timeline
    /// never loaded.
    pub open_post_detail: Rc<dyn Fn(String)>,
    /// Switches Contacts to its People segment, which lists the pending knocks.
    pub switch_to_people_segment: Rc<dyn Fn()>,
    /// The sidebar, for the Family gate and its selection.
    pub sidebar_list_box: gtk::ListBox,
}

/// Wire row activation on the notifications list.
pub fn connect_row_activation(
    list_box: &gtk::ListBox,
    destinations: &Destinations,
    nav: NotificationNav,
) {
    let destinations = Rc::clone(destinations);
    list_box.connect_row_activated(move |_, row| {
        let idx = row.index().max(0) as usize;
        let dest = destinations.borrow().get(idx).cloned().flatten();
        let gated = crate::views::sidebar::family_sidebar_row_shown(&nav.sidebar_list_box);
        if !navigable(&dest, gated) {
            return;
        }
        match dest {
            Some(NotificationDestination::Post { post_id }) => {
                nav.stack.set_visible_child_name("feed");
                (nav.open_post_detail)(post_id);
            }
            Some(NotificationDestination::Knock { .. }) => {
                nav.stack.set_visible_child_name("contacts");
                (nav.switch_to_people_segment)();
            }
            Some(NotificationDestination::Family) => {
                nav.stack.set_visible_child_name("family");
                let family_index = crate::views::sidebar::SidebarItem::ALL.len() as i32 + 1;
                if let Some(row) = nav.sidebar_list_box.row_at_index(family_index) {
                    nav.sidebar_list_box.select_row(Some(&row));
                }
            }
            Some(NotificationDestination::External { url }) => {
                crate::url_opener::open(&url);
            }
            None => {}
        }
    });
}

pub fn build_notifications_view() -> (gtk::Box, NotificationHandles) {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let header = adw::HeaderBar::new();
    let title_label = gtk::Label::new(Some(common::NOTIFICATIONS));
    crate::testid::set_test_id(&title_label, ids::PAGE_HEADING);
    header.set_title_widget(Some(&title_label));

    let mark_read_btn = gtk::Button::with_label(common::MARK_ALL_READ);
    mark_read_btn.add_css_class("flat");
    crate::testid::set_test_id(&mark_read_btn, ids::NOTIFICATION_MARK_READ);
    header.pack_end(&mark_read_btn);

    let clear_btn = gtk::Button::with_label(crate::i18n::strings::notifications::CLEAR_ALL);
    clear_btn.add_css_class("flat");
    crate::testid::set_test_id(&clear_btn, ids::NOTIFICATION_CLEAR);
    header.pack_end(&clear_btn);

    let count_badge = gtk::Label::new(Some(&common::unread_count("0")));
    count_badge.add_css_class("badge");
    crate::testid::set_test_id(&count_badge, ids::NOTIFICATION_COUNT_BADGE);
    header.pack_start(&count_badge);

    // Both page-wide actions are painted blocked still loading until the
    // page's first list read answers; `PageActions::connect` wires the
    // presses once the window's `error-message` exists.
    let actions = PageActions::new(mark_read_btn, clear_btn);

    outer.append(&header);

    let list_box = gtk::ListBox::new();
    list_box.set_selection_mode(gtk::SelectionMode::None);
    list_box.add_css_class("boxed-list");

    let placeholder = adw::StatusPage::builder()
        .title(common::NO_NOTIFICATIONS)
        .icon_name("bell-symbolic")
        .build();
    list_box.set_placeholder(Some(&placeholder));

    let scrolled = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&list_box)
        .build();
    outer.append(&scrolled);

    let handles = NotificationHandles {
        count_badge,
        list_box,
        destinations: Rc::new(RefCell::new(Vec::new())),
        actions,
    };
    (outer, handles)
}

/// Rebuild the notifications list from the rows a `fauna.notifications.list` fetch
/// returned — the notifications twin of `contacts::list::update_knocks_list`.
///
/// Until 2026-07-12 nothing called [`build_notification_row`] at all: the page was a
/// permanently-empty shell (the placeholder + a hard-coded "0 unread" badge), and
/// `NotificationsLoaded` only counted unread rows to fire desktop toasts. So a
/// notification never appeared on the page on *any* path — push, navigation, or
/// reconnect. This is the missing half.
///
/// Called from every `NotificationsLoaded` (login fetch, the `fauna.notification`
/// push arm, `ResyncRequired`, `Reconnected`), so a notification arriving while the
/// user sits on the page shows up with no navigation — the same liveness the web SPA
/// gets from its push arm (`transport.md` § Push events).
///
/// Each row says what the shared decision picks — the localized body, the
/// English `summary`, or the default ([`crate::i18n::notification_text`]).
/// `created_at` is epoch-**microseconds** on the unified-notification wire shape;
/// [`crate::client::format_epoch_us`] resolves the relative-time bucket through
/// shared Rust (`fauna_core::format::relative_time`), never hand-rolled here.
/// Returns the unread count so the caller can keep `AppState` in step.
#[allow(clippy::too_many_arguments)]
pub fn update_notifications_list(
    list_box: &gtk::ListBox,
    count_badge: &gtk::Label,
    destinations: &Destinations,
    family_gated_in: bool,
    items: &[fauna_client_notifications::notifications::NotifItem],
    actions: &PageActions,
    client: &Rc<FaunaClient>,
) -> u32 {
    crate::views::layout::clear_list_box_rows(list_box);
    let mut painted = Vec::with_capacity(items.len());

    // The muted-word collapse (§ Muted words): the shared decision over each
    // row's relayed text, against the one in-memory list the conversation
    // collapse reads (preloaded at login, refreshed by the Settings page).
    let muted = crate::conversations::muted_keywords_cache();
    let mut unread = 0u32;
    for (index, item) in items.iter().enumerate() {
        // The unread badge counts every unread row, muted or not (rule 4).
        if !item.is_read {
            unread += 1;
        }
        let revealed = REVEALED_MUTED_NOTIFICATIONS.with(|s| s.borrow().contains(&item.id));
        if !revealed && fauna_client_notifications::notification_muted(&muted, item) {
            // A collapsed row is no tap target: it holds no destination.
            painted.push(None);
            list_box.append(&build_muted_row(
                list_box,
                destinations,
                family_gated_in,
                index,
                item,
                client,
            ));
            continue;
        }
        let (row, dest) = build_full_row(item, family_gated_in, client);
        painted.push(dest);
        list_box.append(&row);
    }
    *destinations.borrow_mut() = painted;

    // Every loaded row counts toward the clear bound, a collapsed one too
    // (§ Muted words rule 4). The read has answered: the page-wide actions
    // repaint from their shared answers.
    actions.loaded(items.len(), items.iter().map(|i| i.created_at).max());

    count_badge.set_text(&common::unread_count(&unread.to_string()));
    unread
}

/// One open row — its shared text, and the destination it is activatable for.
fn build_full_row(
    item: &fauna_client_notifications::notifications::NotifItem,
    family_gated_in: bool,
    client: &Rc<FaunaClient>,
) -> (gtk::ListBoxRow, Option<NotificationDestination>) {
    let row = build_notification_row(
        &item.notif_type,
        &crate::i18n::notification_text(item),
        &crate::client::format_epoch_us(item.created_at),
        &dismiss_button(item, client),
    );
    // Where the row leads is shared Rust's call, never a local type table.
    let dest = fauna_client_notifications::notification_destination(item);
    row.set_activatable(navigable(&dest, family_gated_in));
    (row, dest)
}

/// `notification-dismiss` for one row — greyed on a security notice inside its
/// retention window by the shared predicate (`notifications.md` § Retention
/// rule 2; the nest's refusal stays the authority). Every row carries one,
/// collapsed or open (§ Muted words rule 4).
fn dismiss_button(
    item: &fauna_client_notifications::notifications::NotifItem,
    client: &Rc<FaunaClient>,
) -> gtk::Button {
    let now_us = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_micros()).unwrap_or(i64::MAX));
    let button = gtk::Button::with_label(crate::i18n::strings::notifications::DISMISS);
    button.add_css_class("flat");
    button.set_sensitive(!fauna_client_notifications::notification_retained(
        item, now_us,
    ));
    crate::testid::set_test_id(&button, ids::NOTIFICATION_DISMISS);
    let (client, id) = (Rc::clone(client), item.id);
    button.connect_clicked(move |_| client.dismiss_notification(id));
    button
}

/// A muted-collapsed row (§ Muted words rule 3): the type icon, the placeholder
/// in place of the sentence, the timestamp, the dismiss and the one other
/// gesture — reveal, which swaps the full row in at the same index for this
/// session. Not activatable and not a `notification-item`, so a muted sender's
/// name never paints as a control.
fn build_muted_row(
    list_box: &gtk::ListBox,
    destinations: &Destinations,
    family_gated_in: bool,
    index: usize,
    item: &fauna_client_notifications::notifications::NotifItem,
    client: &Rc<FaunaClient>,
) -> gtk::ListBoxRow {
    let top_line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    top_line.set_margin_top(8);
    top_line.set_margin_bottom(8);
    top_line.set_margin_start(12);
    top_line.set_margin_end(12);

    let type_icon = gtk::Label::new(Some(notification_type_glyph(&item.notif_type)));
    crate::testid::set_test_id(&type_icon, ids::NOTIFICATION_TYPE_ICON);
    top_line.append(&type_icon);

    let placeholder = gtk::Label::new(Some(crate::i18n::strings::muted_words::COLLAPSED));
    placeholder.add_css_class("dim-label");
    placeholder.set_halign(gtk::Align::Start);
    placeholder.set_hexpand(true);
    crate::testid::set_test_id(&placeholder, ids::NOTIFICATION_MUTED);
    top_line.append(&placeholder);

    let reveal = gtk::Button::with_label(crate::i18n::strings::muted_words::REVEAL);
    reveal.add_css_class("flat");
    crate::testid::set_test_id(&reveal, ids::NOTIFICATION_MUTED_REVEAL_BUTTON);
    top_line.append(&reveal);

    let time_label = gtk::Label::new(Some(&crate::client::format_epoch_us(item.created_at)));
    time_label.add_css_class("dim-label");
    time_label.add_css_class("caption");
    top_line.append(&time_label);
    top_line.append(&dismiss_button(item, client));

    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&top_line));
    row.set_activatable(false);
    {
        let item = item.clone();
        let client = Rc::clone(client);
        let list_box = list_box.clone();
        let destinations = Rc::clone(destinations);
        // Weak: the row owns this button through its child.
        let row_weak = glib::object::ObjectExt::downgrade(&row);
        reveal.connect_clicked(move |_| {
            REVEALED_MUTED_NOTIFICATIONS.with(|s| s.borrow_mut().insert(item.id));
            let Some(row) = row_weak.upgrade() else {
                return;
            };
            let (full, dest) = build_full_row(&item, family_gated_in, &client);
            list_box.remove(&row);
            list_box.insert(&full, index as i32);
            if let Some(slot) = destinations.borrow_mut().get_mut(index) {
                *slot = dest;
            }
        });
    }
    row
}

/// The glyph linux paints for a notification's type — delegates to the shared
/// `fauna_core::notification_glyph` classification web also uses via wasm, so
/// the two apps read the same at a glance (priority #1).
fn notification_type_glyph(notif_type: &fauna_protocol::notifications::NotifType) -> &'static str {
    fauna_core::notification_glyph::NotificationGlyph::of(notif_type).emoji()
}

/// Build a single notification row for the list box.
pub fn build_notification_row(
    notif_type: &fauna_protocol::notifications::NotifType,
    body: &str,
    timestamp: &str,
    dismiss: &gtk::Button,
) -> gtk::ListBoxRow {
    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 4);
    vbox.set_margin_top(8);
    vbox.set_margin_bottom(8);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    let top_line = gtk::Box::new(gtk::Orientation::Horizontal, 8);

    // A `gtk::Label`, not `gtk::Image` (the prior widget): ui.yaml registers
    // `notification-type-icon` as `type: text` and the e2e driver reads it via
    // `get_text` — an icon-only widget with no accessible text can never satisfy
    // that contract (verified 2026-07-17: it always resolved empty, regardless of
    // notification content — a real bug, not a test-writing gap).
    let type_icon = gtk::Label::new(Some(notification_type_glyph(notif_type)));
    crate::testid::set_test_id(&type_icon, ids::NOTIFICATION_TYPE_ICON);
    top_line.append(&type_icon);

    let body_label = gtk::Label::new(Some(body));
    body_label.set_halign(gtk::Align::Start);
    body_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    body_label.set_max_width_chars(80);
    body_label.set_hexpand(true);
    top_line.append(&body_label);

    let time_label = gtk::Label::new(Some(timestamp));
    time_label.set_halign(gtk::Align::End);
    time_label.add_css_class("dim-label");
    time_label.add_css_class("caption");
    top_line.append(&time_label);

    top_line.append(dismiss);

    vbox.append(&top_line);

    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&vbox));
    crate::testid::set_test_id(&row, ids::NOTIFICATION_ITEM);
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reason(key: &str) -> String {
        fauna_core::localized::LocalizedText::key(key).resolve(crate::i18n::strings::lookup)
    }

    fn actions() -> PageActions {
        PageActions::new(gtk::Button::new(), gtk::Button::new())
    }

    /// Unknown is not possible (`notifications.md` § Errors & edge cases):
    /// before the first list read answers, *Mark all read* and *Clear all*
    /// are blocked still loading (reason beside them) and their presses
    /// refuse onto `error-message` without running; once read, an empty page
    /// blocks *Clear all* with its reason and a row enables it.
    #[test]
    fn the_page_wide_actions_wait_for_the_first_read() {
        crate::testid::run_on_gtk_thread(|| {
            let a = actions();
            let error = gtk::Label::new(None);
            let marked = Rc::new(Cell::new(false));
            let cleared = Rc::new(Cell::new(None::<i64>));
            {
                let marked = Rc::clone(&marked);
                let cleared = Rc::clone(&cleared);
                a.connect(
                    &error,
                    move || marked.set(true),
                    move |b| cleared.set(Some(b)),
                );
            }
            let loading = reason("common.still_loading");
            for btn in [&a.mark_read_button, &a.clear_button] {
                assert!(btn.is_visible() && !btn.is_sensitive());
                assert_eq!(btn.tooltip_text().as_deref(), Some(loading.as_str()));
            }
            a.mark_read_button.emit_clicked();
            assert_eq!(error.text().as_str(), loading);
            assert!(!marked.get(), "a refused press runs nothing");
            a.clear_button.emit_clicked();
            assert_eq!(cleared.get(), None);

            a.loaded(0, None);
            assert!(a.mark_read_button.is_sensitive());
            assert!(!a.clear_button.is_sensitive());
            let nothing = reason("notifications.nothing_to_clear");
            assert_eq!(
                a.clear_button.tooltip_text().as_deref(),
                Some(nothing.as_str())
            );
            a.clear_button.emit_clicked();
            assert_eq!(error.text().as_str(), nothing);
            a.mark_read_button.emit_clicked();
            assert!(marked.get());

            a.loaded(1, Some(42));
            assert!(a.clear_button.is_sensitive());
            a.clear_button.emit_clicked();
            assert_eq!(cleared.get(), Some(42));
        });
    }

    /// A failed first read blocks both with load-failed; a failure after a
    /// resolved read keeps the rows already shown actionable.
    #[test]
    fn a_failed_read_blocks_until_a_read_resolves() {
        crate::testid::run_on_gtk_thread(|| {
            let a = actions();
            a.read_failed();
            let failed = reason("common.load_failed");
            assert!(!a.mark_read_button.is_sensitive());
            assert_eq!(
                a.mark_read_button.tooltip_text().as_deref(),
                Some(failed.as_str())
            );
            a.loaded(2, Some(7));
            a.read_failed();
            assert!(a.mark_read_button.is_sensitive());
            assert!(a.clear_button.is_sensitive());
        });
    }
}
