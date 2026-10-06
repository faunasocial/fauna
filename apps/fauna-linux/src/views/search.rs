//! The Search page — a **paint shell** over the shared
//! [`fauna_client_search::SearchManager`] (`docs/goal/ui/search.md` § State &
//! data shape; ui.yaml `search`), mirroring `crate::views::feed`'s
//! manager+observer shape (`crate::search::host` / `crate::search::observer`).
//!
//! Every decision this page used to make itself — which backends to ask, how
//! to map the type filter onto them, when to offer "load more", what the
//! badge and snippet say — now belongs to the manager. What is left here is
//! genuinely per-app: the GTK widgets and the toggle/cancel bar-visibility
//! local state (`wire_search_bar_controls`, unchanged in shape from before
//! the adoption).

use fauna_ui_ids as ids;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;

use fauna_client_search::{SearchNav, TYPE_FILTER_OPTIONS};

use crate::client::FaunaClient;
use crate::i18n::strings::{common, search_page};
use crate::search::host::LinuxSearchManager;
use crate::search::observer;

/// The cross-page navigation a `search-result-item` activation dispatches
/// into, one per [`SearchNav`] destination (`ui/search.md` § Where logic
/// lives → Result navigation (deep link); mirrors tui's `open_result` —
/// `apps/fauna-tui/src/search.rs`).
///
/// Every field is a handle onto state built (once, at login) by the page it
/// belongs to, threaded in here rather than re-derived — search never re-opens
/// a nest connection or re-derives a manager of its own.
pub struct SearchNavHandles {
    /// The main content stack — switched to the destination page's named
    /// child before the destination-specific open runs, so a search-only
    /// visitor lands somewhere real rather than the target painting off-screen.
    pub stack: gtk::Stack,
    /// Opens the Feed post-detail pane for a post named only by id
    /// (`views::feed::FeedViewHandles::open_post_detail`).
    pub open_post_detail: Rc<dyn Fn(String)>,
    /// Switches the Contacts page to its Address Book segment without the
    /// segment toggle's own `fetch_addressbooks` side effect
    /// (`views::contacts::build_contacts_view`'s `switch_to_addressbook_segment`).
    pub switch_to_addressbook_segment: Rc<dyn Fn()>,
    /// The shared `MediaMachine`, for `MediaMachine::locate_file` +
    /// `views::media::detail::open_item_detail`.
    pub media_machine: Arc<fauna_media_machine::MediaMachine>,
}

/// i18n label for a `search-type-filter` token, resolved from the shared map
/// ([`fauna_client_search::type_filter_label`]) rather than a per-app match.
///
/// An option is labelled with the very badge its rows carry, so the picker and
/// its own results can never read differently; and a token this build has not
/// been taught surfaces raw instead of rendering a *second* entry reading
/// "All", which is what the hand-rolled `_ => ALL` arm used to do.
fn type_filter_label(token: &str) -> String {
    fauna_client_search::type_filter_label(token).resolve(crate::i18n::strings::lookup)
}

/// Widget handles for the search view.
pub struct SearchHandles {
    /// The search entry widget — `app.rs` wires a Ctrl+F focus shortcut to it.
    pub search_entry: gtk::SearchEntry,
}

/// Build the full search view: search entry + type filter at the top, results
/// list below, rendered entirely from the shared `SearchManager` snapshot via
/// a `SearchSnapshotObserver`. The manager must already be initialised
/// (`crate::search::host::init`) — `build_main_window` does that right before
/// calling this. `nav` is the cross-page navigation `search-result-item`
/// activation dispatches into (see [`SearchNavHandles`]).
pub fn build_search_view(
    client: &Rc<FaunaClient>,
    nav: SearchNavHandles,
) -> (gtk::Box, SearchHandles) {
    let manager = crate::search::host::manager()
        .expect("search manager must be initialised before build_search_view");
    let rt = client.runtime_handle();

    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);

    // Header with search entry and submit button.
    let header = adw::HeaderBar::new();
    // Page heading marker for E2E test consistency.
    let heading_marker = gtk::Label::new(Some(search_page::PLACEHOLDER));
    heading_marker.set_height_request(1);
    heading_marker.set_overflow(gtk::Overflow::Hidden);
    crate::testid::set_test_id(&heading_marker, ids::PAGE_HEADING);
    header.pack_start(&heading_marker);

    let search_entry = gtk::SearchEntry::new();
    search_entry.set_placeholder_text(Some(search_page::PLACEHOLDER));
    search_entry.set_hexpand(true);
    crate::testid::set_test_id(&search_entry, ids::SEARCH_QUERY_FIELD);
    // Enter in the entry fires the same query as the submit button
    // (`fire_search`, wired below) — tui's `Action::Submit`.
    crate::offline_gate::declare_wire_kind(&search_entry, "fauna.search.query");
    header.set_title_widget(Some(&search_entry));

    let submit_btn = gtk::Button::from_icon_name("edit-find-symbolic");
    submit_btn.set_tooltip_text(Some(common::SEARCH));
    crate::testid::set_test_id(&submit_btn, ids::SEARCH_SUBMIT_BUTTON);
    crate::offline_gate::declare_wire_kind(&submit_btn, "fauna.search.query");
    header.pack_end(&submit_btn);

    // Clear button for the search entry.
    let clear_btn = gtk::Button::from_icon_name("edit-clear-symbolic");
    clear_btn.add_css_class("flat");
    clear_btn.set_tooltip_text(Some(common::CLEAR_SEARCH));
    crate::testid::set_test_id(&clear_btn, ids::SEARCH_CLEAR_BUTTON);
    header.pack_end(&clear_btn);

    {
        let entry = search_entry.clone();
        clear_btn.connect_clicked(move |_| {
            entry.set_text("");
        });
    }

    // Collapses/reveals the search bar row (entry + type filter + submit +
    // clear) — mirrors web's `barVisible` toggle (`routes/search/+page.svelte`).
    let toggle_btn = gtk::Button::from_icon_name("pan-up-symbolic");
    toggle_btn.add_css_class("flat");
    toggle_btn.set_tooltip_text(Some(search_page::HIDE_SEARCH_BAR));
    crate::testid::set_test_id(&toggle_btn, ids::SEARCH_TOGGLE_BUTTON);
    header.pack_start(&toggle_btn);

    // Cancels the current search: clears query/results and re-shows the bar.
    // Only shown once a search has actually fired (mirrors web's `{#if
    // searched}`), same as `search-clear-button`'s query-only reset.
    let cancel_btn = gtk::Button::with_label(common::CANCEL);
    cancel_btn.set_visible(false);
    crate::testid::set_test_id(&cancel_btn, ids::SEARCH_CANCEL_BUTTON);
    header.pack_start(&cancel_btn);

    outer.append(&header);

    // Content-type filter bar.
    let filter_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    filter_bar.set_margin_start(12);
    filter_bar.set_margin_end(12);
    filter_bar.set_margin_top(6);
    filter_bar.set_margin_bottom(6);

    let filter_label = gtk::Label::new(Some(common::TYPE));
    filter_label.add_css_class("dim-label");
    filter_bar.append(&filter_label);

    // The shared token list (`fauna_client_search::TYPE_FILTER_OPTIONS`), not a
    // per-app copy — the option list, the nest parameter, and the local kind
    // set can never drift apart (`ui/search.md` § Implementation status today).
    let labels: Vec<String> = TYPE_FILTER_OPTIONS
        .iter()
        .map(|t| type_filter_label(t))
        .collect();
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let string_list = gtk::StringList::new(&label_refs);
    let type_dropdown = gtk::DropDown::new(Some(string_list), gtk::Expression::NONE);
    type_dropdown.set_selected(0);
    crate::testid::set_test_id(&type_dropdown, ids::SEARCH_TYPE_FILTER);
    // Selection change re-fires the query with the live buffer text — tui's
    // `Action::SetTypeFilter`.
    crate::offline_gate::declare_wire_kind(&type_dropdown, "fauna.search.query");
    filter_bar.append(&type_dropdown);

    outer.append(&filter_bar);

    // error-message — every page has one (Rule 2), hidden until the snapshot
    // carries an error. A failed arm sets it WHILE the other arm's rows stay
    // painted (`search.md` § The local/nest merge: partial results shown
    // honestly, never blanked) — tui's `sync_error`.
    let error_label = gtk::Label::builder().visible(false).wrap(true).build();
    error_label.add_css_class("error");
    error_label.set_margin_start(12);
    error_label.set_margin_end(12);
    error_label.set_halign(gtk::Align::Start);
    crate::testid::set_test_id(&error_label, ids::ERROR_MESSAGE);
    outer.append(&error_label);

    // Results list.
    let list_box = gtk::ListBox::new();
    list_box.set_selection_mode(gtk::SelectionMode::None);
    list_box.set_activate_on_single_click(true);
    list_box.add_css_class("boxed-list");
    crate::testid::set_test_id(&list_box, ids::SEARCH_RESULTS_VIEW);

    // Row activation → route by the row's typed `SearchNav` target
    // (`ui/search.md` § User actions — "search-result-item[i] | Open
    // destination"; mirrors tui's `open_result`). Rows are appended 1:1 with
    // `snapshot.results` in `refresh` (the `post-card` idiom), so the row's
    // index is read against a FRESH snapshot at click time.
    {
        let manager = Arc::clone(&manager);
        let stack = nav.stack.clone();
        let open_post_detail = Rc::clone(&nav.open_post_detail);
        let switch_to_addressbook_segment = Rc::clone(&nav.switch_to_addressbook_segment);
        let media_machine = Arc::clone(&nav.media_machine);
        let client = Rc::clone(client);
        let rt = rt.clone();
        list_box.connect_row_activated(move |_, row| {
            let idx = row.index().max(0) as usize;
            let Some(target) = manager
                .snapshot()
                .results
                .get(idx)
                .and_then(|r| r.navigation.clone())
            else {
                // No typed destination (or the row list changed under the
                // click) — an inert row, same posture ui.yaml's
                // `search-result-item` description gives: "a row whose target
                // has no destination yet renders inert."
                return;
            };
            open_search_result(
                target,
                &stack,
                &open_post_detail,
                &switch_to_addressbook_segment,
                &media_machine,
                &client,
                &rt,
            );
        });
    }

    // No-results indicator. A GtkListBox *placeholder* (the old approach) is
    // not surfaced to AT-SPI, so `is_visible("search-no-results")` always read
    // false; an explicit plain `gtk::Label` we show/hide on each refresh is
    // discoverable. Hidden until a search actually returns zero rows.
    let no_results_label = gtk::Label::new(Some(search_page::NO_RESULTS_SHORT));
    no_results_label.add_css_class("dim-label");
    no_results_label.set_margin_top(24);
    no_results_label.set_halign(gtk::Align::Center);
    no_results_label.set_visible(false);
    crate::testid::set_test_id(&no_results_label, ids::SEARCH_NO_RESULTS);

    // Wrap the list box in a vertical container so we can stack the no-results
    // label and the "Load more" button below it inside the same scroll region.
    let results_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    results_box.append(&list_box);
    results_box.append(&no_results_label);

    let load_more_btn = gtk::Button::with_label(common::LOAD_MORE);
    load_more_btn.set_halign(gtk::Align::Center);
    load_more_btn.set_margin_top(8);
    load_more_btn.set_margin_bottom(8);
    load_more_btn.set_visible(false);
    crate::testid::set_test_id(&load_more_btn, ids::SEARCH_LOAD_MORE_BUTTON);
    // Pages the same query — tui's `Action::LoadMore`.
    crate::offline_gate::declare_wire_kind(&load_more_btn, "fauna.search.query");
    results_box.append(&load_more_btn);

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&results_box)
        .build();

    outer.append(&scrolled);

    // Helper closure: fire the manager using the live entry text + selected
    // type — the manager owns query/filter/paging/merge from here.
    let fire_search = {
        let manager = Arc::clone(&manager);
        let entry = search_entry.clone();
        let dropdown = type_dropdown.clone();
        let rt = rt.clone();
        Rc::new(move || {
            let query = entry.text().to_string();
            let query = query.trim().to_string();
            if query.is_empty() {
                return;
            }
            let idx = dropdown.selected() as usize;
            let token = TYPE_FILTER_OPTIONS
                .get(idx)
                .copied()
                .unwrap_or(fauna_client_search::TYPE_FILTER_ALL)
                .to_string();
            let manager = Arc::clone(&manager);
            rt.spawn(async move {
                manager.run_query(&query, &token).await;
            });
        })
    };

    // Wire the load-more button: the manager bumps its own limit and re-fires.
    {
        let manager = Arc::clone(&manager);
        let rt = rt.clone();
        load_more_btn.connect_clicked(move |_| {
            let manager = Arc::clone(&manager);
            rt.spawn(async move {
                manager.load_more().await;
            });
        });
    }

    // Wire search entry activation.
    {
        let fire = Rc::clone(&fire_search);
        search_entry.connect_activate(move |_| {
            fire();
        });
    }

    // Wire submit button click.
    {
        let fire = Rc::clone(&fire_search);
        submit_btn.connect_clicked(move |_| {
            fire();
        });
    }

    // Wire dropdown change — re-run search when the filter changes.
    {
        let fire = Rc::clone(&fire_search);
        type_dropdown.connect_selected_notify(move |_| {
            fire();
        });
    }

    wire_search_bar_controls(
        &toggle_btn,
        &cancel_btn,
        &search_entry,
        &submit_btn,
        &clear_btn,
        &filter_bar,
        &type_dropdown,
        &manager,
    );

    // ── Observer → main-thread refresh loop ──────────────────────────────
    {
        let manager_loop = Arc::clone(&manager);
        let list_box = list_box.clone();
        let no_results_label = no_results_label.clone();
        let load_more_btn = load_more_btn.clone();
        let cancel_btn = cancel_btn.clone();
        let error_label = error_label.clone();
        let rx = observer::attach(&manager);
        // Initial render so the empty state shows before the first mutation.
        refresh(
            &manager_loop,
            &list_box,
            &no_results_label,
            &load_more_btn,
            &cancel_btn,
            &error_label,
        );
        let mut was_rooted = false;
        crate::async_helper::spawn_wake_loop(rx, move || {
            // Self-terminate once this view's window is torn down, so a
            // re-auth's fresh observer loop doesn't race a dead one.
            if list_box.root().is_some() {
                was_rooted = true;
            } else if was_rooted {
                return glib::ControlFlow::Break;
            }
            refresh(
                &manager_loop,
                &list_box,
                &no_results_label,
                &load_more_btn,
                &cancel_btn,
                &error_label,
            );
            glib::ControlFlow::Continue
        });
    }

    let handles = SearchHandles { search_entry };

    (outer, handles)
}

/// Route an activated `search-result-item`'s typed [`SearchNav`] target into
/// its destination — the door every arm goes through
/// (`ui/search.md` § Where logic lives → Result navigation (deep link);
/// mirrors tui's `open_result`, `apps/fauna-tui/src/search.rs`). Every arm
/// switches the main content stack to the destination page FIRST, so the
/// destination-specific open (a resolve, a locate, a lookup) always lands on
/// a visible page rather than painting off-screen.
#[allow(clippy::too_many_arguments)]
fn open_search_result(
    nav: SearchNav,
    stack: &gtk::Stack,
    open_post_detail: &Rc<dyn Fn(String)>,
    switch_to_addressbook_segment: &Rc<dyn Fn()>,
    media_machine: &Arc<fauna_media_machine::MediaMachine>,
    client: &Rc<FaunaClient>,
    rt: &tokio::runtime::Handle,
) {
    match nav {
        SearchNav::Post { post_id } => {
            stack.set_visible_child_name("feed");
            open_post_detail(post_id);
        }
        // `Mail` lands the WHOLE contract (thread jump + message selection —
        // `ui/search.md` § State & data shape; `conversations.md` § The
        // selected message): `select_thread_and_message` sets both under one
        // `notify`, so the detail render never sees the thread flipped with no
        // message marked yet. The paint (mark + `selected` attribute +
        // scroll-into-view) lives in `views/conversations/{message_bubble,detail}.rs`.
        SearchNav::Mail {
            thread_id,
            message_id,
        } => {
            stack.set_visible_child_name("conversations");
            crate::conversations::manager().select_thread_and_message(
                fauna_conversations::ThreadId(thread_id),
                fauna_conversations::message::MessageId(message_id),
            );
        }
        SearchNav::Draft {
            thread_id: Some(thread_id),
        } => {
            stack.set_visible_child_name("conversations");
            crate::conversations::manager().select_thread(fauna_conversations::ThreadId(thread_id));
        }
        SearchNav::Draft { thread_id: None } => {
            stack.set_visible_child_name("conversations");
            crate::conversations::manager().start_new_conversation();
        }
        SearchNav::Contact { uid_hash } => {
            // Switch FIRST (synchronously): the locate below is a round trip,
            // and landing on the People segment first would flash the wrong
            // half of the page before the reply lands.
            stack.set_visible_child_name("contacts");
            switch_to_addressbook_segment();
            client.locate_card_by_uid(&uid_hash);
        }
        SearchNav::File {
            folder_id,
            path_hash,
        } => {
            stack.set_visible_child_name("media");
            // `locate_file` runs over the raw drained aggregate — independent
            // of the page's active `media-folder-filter` and sort
            // (`MediaMachine::locate_file`'s own doc comment owns why). A
            // `None` (deleted, renamed, or in a set this actor cannot see)
            // degrades to the navigate-with-nothing-open posture tui takes
            // for the same case.
            if let Some(item) = media_machine.locate_file(folder_id, path_hash) {
                let parent = stack.root().and_then(|r| r.downcast::<gtk::Window>().ok());
                // The owner key the detail's download walk takes for an
                // owner-only set — the same derivation the Media page's cards use.
                let backup_key = fauna_core::crypto::BackupKey::derive(&client.secret_bytes())
                    .to_bytes()
                    .to_vec();
                crate::views::media::detail::open_item_detail(
                    parent.as_ref(),
                    &item,
                    media_machine,
                    &backup_key,
                    rt,
                );
            }
        }
    }
}

/// Re-read the snapshot and re-render the results pane, the page error, and
/// the cancel button's searched-gated visibility.
fn refresh(
    manager: &Arc<LinuxSearchManager>,
    list_box: &gtk::ListBox,
    no_results_label: &gtk::Label,
    load_more_btn: &gtk::Button,
    cancel_btn: &gtk::Button,
    error_label: &gtk::Label,
) {
    let snap = manager.snapshot();

    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }
    for item in &snap.results {
        let timestamp = crate::client::format_epoch_us(item.timestamp * 1_000);
        let row = build_search_result_row(&item.badge, &item.snippet, &timestamp);
        list_box.append(&row);
    }

    // The three states are mutually exclusive by construction: `no_results` is
    // only ever set on a settled empty result set, so a first query still in
    // flight shows neither (`search.md` § State & data shape).
    no_results_label.set_visible(snap.no_results);
    load_more_btn.set_visible(snap.has_more);
    // Only shown once a search has actually fired (web's `{#if searched}`).
    cancel_btn.set_visible(!snap.query.is_empty());
    // Independent of the rows above: a failed arm keeps the other's rows.
    match &snap.error {
        Some(text) => {
            error_label.set_text(&text.resolve(crate::i18n::strings::lookup));
            error_label.set_visible(true);
        }
        None => {
            error_label.set_text("");
            error_label.set_visible(false);
        }
    }
}

/// Build a search result row: content type badge, snippet, timestamp.
fn build_search_result_row(
    badge: &fauna_core::localized::LocalizedText,
    snippet: &str,
    timestamp: &str,
) -> gtk::ListBoxRow {
    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 4);
    vbox.set_margin_top(8);
    vbox.set_margin_bottom(8);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    // Top line: content type badge + timestamp.
    let top_line = gtk::Box::new(gtk::Orientation::Horizontal, 8);

    let badge_text = badge.resolve(crate::i18n::strings::lookup);
    let badge_label = gtk::Label::new(Some(&badge_text));
    badge_label.set_halign(gtk::Align::Start);
    badge_label.add_css_class("heading");
    badge_label.add_css_class("caption");
    top_line.append(&badge_label);

    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    top_line.append(&spacer);

    let time_label = gtk::Label::new(Some(timestamp));
    time_label.set_halign(gtk::Align::End);
    time_label.add_css_class("dim-label");
    time_label.add_css_class("caption");
    top_line.append(&time_label);

    vbox.append(&top_line);

    // Snippet — already cleaned by the manager (shared FTS-marker strip + entity
    // decode, `fauna_client_search::render::clean_snippet`), never re-cleaned here.
    let snippet_label = gtk::Label::new(Some(snippet));
    snippet_label.set_halign(gtk::Align::Start);
    snippet_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    snippet_label.set_max_width_chars(120);
    snippet_label.set_lines(2);
    snippet_label.set_wrap(true);
    snippet_label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    snippet_label.add_css_class("dim-label");
    vbox.append(&snippet_label);

    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&vbox));
    crate::testid::set_test_id(&row, ids::SEARCH_RESULT_ITEM);
    // Activation routes into `views::media::detail::open_item_detail` for a
    // file target (`SearchNav::File`) — same kind as media's own file-detail
    // open action (`views/media/detail.rs`'s `show_pruned_switch`).
    crate::offline_gate::declare_wire_kind(&row, "fauna.files.versions.list");
    row
}

/// Wires `search-toggle-button` (collapse/reveal the entry + type filter +
/// submit + clear row) and `search-cancel-button` (reset the query + manager
/// state and re-show the row, shown only once a search has fired) — mirrors
/// web's `barVisible`/`searched` handlers (`routes/search/+page.svelte`).
#[allow(clippy::too_many_arguments)]
fn wire_search_bar_controls(
    toggle_btn: &gtk::Button,
    cancel_btn: &gtk::Button,
    entry: &gtk::SearchEntry,
    submit_btn: &gtk::Button,
    clear_btn: &gtk::Button,
    filter_bar: &gtk::Box,
    type_dropdown: &gtk::DropDown,
    manager: &Arc<LinuxSearchManager>,
) {
    // Shared by both handlers so a cancel-forced re-show can't desync from
    // the toggle button's own idea of the bar's visibility.
    let bar_visible = Rc::new(Cell::new(true));

    // Apply a bar-visible state to the entry/submit/clear/filter row and the
    // toggle button's icon + tooltip. Shared by the toggle and cancel
    // handlers so they can never disagree about the current state.
    let apply_bar_visible = {
        let entry = entry.clone();
        let submit = submit_btn.clone();
        let clear = clear_btn.clone();
        let filters = filter_bar.clone();
        let toggle = toggle_btn.clone();
        let bar_visible = Rc::clone(&bar_visible);
        Rc::new(move |visible: bool| {
            bar_visible.set(visible);
            entry.set_visible(visible);
            submit.set_visible(visible);
            clear.set_visible(visible);
            filters.set_visible(visible);
            toggle.set_icon_name(if visible {
                "pan-up-symbolic"
            } else {
                "pan-down-symbolic"
            });
            toggle.set_tooltip_text(Some(if visible {
                search_page::HIDE_SEARCH_BAR
            } else {
                search_page::SHOW_SEARCH_BAR
            }));
        })
    };

    // Wire the toggle button: collapse/reveal the entry + type filter +
    // submit + clear row, mirroring web's `barVisible` (the results panel is
    // untouched — it stays visible while the bar is hidden, same as web).
    {
        let bar_visible = Rc::clone(&bar_visible);
        let apply_bar_visible = Rc::clone(&apply_bar_visible);
        toggle_btn.connect_clicked(move |_| {
            apply_bar_visible(!bar_visible.get());
        });
    }

    // Wire the cancel button: clear the query + manager state, re-show the
    // bar. The manager's own `cancel()` resets its snapshot and notifies —
    // the observer refresh loop picks up the cleared results/cancel-button
    // visibility from there; only the entry text + dropdown + bar are ours.
    {
        let entry = entry.clone();
        let dropdown = type_dropdown.clone();
        let manager = Arc::clone(manager);
        let cancel = cancel_btn.clone();
        let apply_bar_visible = Rc::clone(&apply_bar_visible);
        cancel_btn.connect_clicked(move |_| {
            entry.set_text("");
            dropdown.set_selected(0);
            manager.cancel();
            cancel.set_visible(false);
            apply_bar_visible(true);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client::NestClient;
    use fauna_client_search::SearchManager;
    use fauna_client_search::snapshot::{SearchResultRow, SearchSnapshot, SearchSource};
    use fauna_core::localized::LocalizedText;

    /// A manager over an unreachable transport. Nothing here ever fires a real
    /// query — these tests drive the page's own wiring (toggle/cancel/render),
    /// and the manager's own behaviour (both arms, merge, ordering, paging) is
    /// pinned by its tests in `fauna-client-search`, not re-driven here — same
    /// stance as tui's `offline_manager` (`apps/fauna-tui/src/search.rs`).
    fn offline_manager() -> Arc<LinuxSearchManager> {
        Arc::new(SearchManager::new(NestClient::new(
            "http://127.0.0.1:1".to_string(),
            fauna_core::identity::ActorKeypair::generate(),
        )))
    }

    fn row(content_type: &str, snippet: &str) -> SearchResultRow {
        SearchResultRow {
            content_id: "c1".into(),
            content_type: content_type.into(),
            badge: fauna_client_search::render::content_type_badge(content_type),
            snippet: snippet.into(),
            timestamp: 0,
            source: SearchSource::Nest,
            navigation: None,
        }
    }

    struct BarWidgets {
        toggle_btn: gtk::Button,
        cancel_btn: gtk::Button,
        entry: gtk::SearchEntry,
        submit_btn: gtk::Button,
        clear_btn: gtk::Button,
        filter_bar: gtk::Box,
        type_dropdown: gtk::DropDown,
        list_box: gtk::ListBox,
        no_results_label: gtk::Label,
        load_more_btn: gtk::Button,
        error_label: gtk::Label,
        manager: Arc<LinuxSearchManager>,
    }

    /// Builds the same bar widgets `build_search_view` would, wired through the
    /// real `wire_search_bar_controls` — no `FaunaClient` needed, since that
    /// function only touches the bar widgets + the manager's sync `cancel()`.
    fn wired_bar() -> BarWidgets {
        let toggle_btn = gtk::Button::from_icon_name("pan-up-symbolic");
        let cancel_btn = gtk::Button::with_label(common::CANCEL);
        let entry = gtk::SearchEntry::new();
        let submit_btn = gtk::Button::new();
        let clear_btn = gtk::Button::new();
        let filter_bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let labels: Vec<String> = TYPE_FILTER_OPTIONS
            .iter()
            .map(|t| type_filter_label(t))
            .collect();
        let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let type_dropdown = gtk::DropDown::new(
            Some(gtk::StringList::new(&label_refs)),
            gtk::Expression::NONE,
        );
        let list_box = gtk::ListBox::new();
        let no_results_label = gtk::Label::new(None);
        let load_more_btn = gtk::Button::new();
        let error_label = gtk::Label::new(None);
        let manager = offline_manager();

        wire_search_bar_controls(
            &toggle_btn,
            &cancel_btn,
            &entry,
            &submit_btn,
            &clear_btn,
            &filter_bar,
            &type_dropdown,
            &manager,
        );

        BarWidgets {
            toggle_btn,
            cancel_btn,
            entry,
            submit_btn,
            clear_btn,
            filter_bar,
            type_dropdown,
            list_box,
            no_results_label,
            load_more_btn,
            error_label,
            manager,
        }
    }

    /// `populate_search_results` (now `refresh`) had no visibility test at
    /// all in the pre-adoption code, which is how the dead-click load-more bug
    /// survived. A partial page hides the affordance; `has_more` from the
    /// snapshot is what drives it now, not a locally re-derived paging policy.
    #[test]
    fn refresh_renders_results_and_derives_visibility_from_the_snapshot() {
        crate::testid::run_on_gtk_thread(|| {
            let w = wired_bar();
            w.manager.set_snapshot_for_test(SearchSnapshot {
                query: "hello".into(),
                results: vec![row("post", "a"), row("post", "b")],
                has_more: true,
                no_results: false,
                ..Default::default()
            });
            refresh(
                &w.manager,
                &w.list_box,
                &w.no_results_label,
                &w.load_more_btn,
                &w.cancel_btn,
                &w.error_label,
            );
            assert_eq!(
                w.list_box.observe_children().n_items(),
                2,
                "both result rows must render"
            );
            assert!(
                w.load_more_btn.is_visible(),
                "has_more=true shows load-more"
            );
            assert!(!w.no_results_label.is_visible());
            assert!(w.cancel_btn.is_visible(), "a non-empty query shows cancel");

            w.manager.set_snapshot_for_test(SearchSnapshot {
                query: "hello".into(),
                results: vec![],
                has_more: false,
                no_results: true,
                ..Default::default()
            });
            refresh(
                &w.manager,
                &w.list_box,
                &w.no_results_label,
                &w.load_more_btn,
                &w.cancel_btn,
                &w.error_label,
            );
            assert_eq!(
                w.list_box.observe_children().n_items(),
                0,
                "cleared on re-render"
            );
            assert!(!w.load_more_btn.is_visible());
            assert!(w.no_results_label.is_visible());
        });
    }

    /// A failed arm sets the snapshot's error while the other arm's rows stay
    /// (`search.md` § The local/nest merge) — the page must say so on
    /// `error-message` AND keep those rows, never one instead of the other.
    /// The view once painted neither, so a failed nest arm read as success.
    #[test]
    fn refresh_shows_a_failed_arm_beside_the_rows_it_kept_and_clears_it_after() {
        crate::testid::run_on_gtk_thread(|| {
            let w = wired_bar();
            w.manager.set_snapshot_for_test(SearchSnapshot {
                query: "hello".into(),
                results: vec![row("draft", "kept")],
                error: Some(LocalizedText::key_arg(
                    "search_page.search_failed_reason",
                    "reason",
                    "nest: deadline",
                )),
                ..Default::default()
            });
            refresh(
                &w.manager,
                &w.list_box,
                &w.no_results_label,
                &w.load_more_btn,
                &w.cancel_btn,
                &w.error_label,
            );
            assert!(
                w.error_label.is_visible(),
                "a failed arm shows error-message"
            );
            let text = w.error_label.text();
            assert!(
                text.starts_with(search_page::SEARCH_FAILED) && text.contains("nest: deadline"),
                "the error names the failure and its reason: {text:?}"
            );
            assert_eq!(
                w.list_box.observe_children().n_items(),
                1,
                "the kept row stays painted beside the error"
            );

            w.manager.set_snapshot_for_test(SearchSnapshot {
                query: "hello".into(),
                results: vec![row("draft", "kept")],
                ..Default::default()
            });
            refresh(
                &w.manager,
                &w.list_box,
                &w.no_results_label,
                &w.load_more_btn,
                &w.cancel_btn,
                &w.error_label,
            );
            assert!(
                !w.error_label.is_visible(),
                "a clean search clears the error"
            );
        });
    }

    /// `search-toggle-button`/`search-cancel-button` used to be inert 1px
    /// `gtk::Label` markers ("desktop doesn't need a toggle/cancel button")
    /// while web/windows/macos/ios/android all built a real collapsible
    /// search bar — this pins the real, wired behavior linux matches.
    #[test]
    fn toggle_button_hides_and_shows_the_bar_row() {
        crate::testid::run_on_gtk_thread(|| {
            let w = wired_bar();
            assert!(w.entry.is_visible());
            assert!(w.submit_btn.is_visible());
            assert!(w.clear_btn.is_visible());
            assert!(w.filter_bar.is_visible());

            w.toggle_btn.emit_clicked();
            assert!(!w.entry.is_visible());
            assert!(!w.submit_btn.is_visible());
            assert!(!w.clear_btn.is_visible());
            assert!(!w.filter_bar.is_visible());

            w.toggle_btn.emit_clicked();
            assert!(
                w.entry.is_visible(),
                "a second toggle click re-shows the bar"
            );
        });
    }

    #[test]
    fn cancel_button_resets_query_dropdown_and_manager_and_re_shows_the_bar() {
        crate::testid::run_on_gtk_thread(|| {
            let w = wired_bar();
            w.entry.set_text("hello");
            w.type_dropdown.set_selected(1);
            w.cancel_btn.set_visible(true);
            w.manager.set_snapshot_for_test(SearchSnapshot {
                query: "hello".into(),
                results: vec![row("post", "a")],
                ..Default::default()
            });

            w.cancel_btn.emit_clicked();

            assert_eq!(w.entry.text(), "");
            assert_eq!(w.type_dropdown.selected(), 0);
            assert!(!w.cancel_btn.is_visible());
            assert!(
                w.manager.snapshot().query.is_empty(),
                "cancel() must reset the manager's own query too"
            );
        });
    }

    /// The bug this shape specifically guards against: hide the bar via
    /// toggle, then cancel (which force-shows it) — a THIRD click on toggle
    /// must hide it again immediately, not silently re-sync-then-no-op.
    #[test]
    fn cancel_after_a_hidden_toggle_does_not_desync_the_next_toggle_click() {
        crate::testid::run_on_gtk_thread(|| {
            let w = wired_bar();
            w.toggle_btn.emit_clicked(); // hide
            assert!(!w.entry.is_visible());

            w.cancel_btn.set_visible(true);
            w.cancel_btn.emit_clicked(); // forces the bar back visible
            assert!(w.entry.is_visible(), "cancel re-shows the bar");

            w.toggle_btn.emit_clicked();
            assert!(
                !w.entry.is_visible(),
                "toggle must hide on the very next click, not resync first"
            );
        });
    }

    /// Every offered option resolves to a real human label — and to the SAME
    /// one the shared map hands every other app, since this is now a thin
    /// resolve over `fauna_client_search::type_filter_label` rather than a
    /// per-app match. A key that failed to resolve would surface as its own
    /// dotted key, so asserting the human string pins the whole chain.
    #[test]
    fn every_option_resolves_to_the_shared_human_label() {
        assert_eq!(type_filter_label("all"), "All");
        assert_eq!(type_filter_label("post"), "Post");
        assert_eq!(type_filter_label("imap"), "Email");
        assert_eq!(type_filter_label("profile"), "Profile");
        for token in TYPE_FILTER_OPTIONS {
            let label = type_filter_label(token);
            assert!(
                !label.is_empty() && !label.contains('.'),
                "{token} must resolve to a human label, got {label:?}"
            );
        }
    }

    /// The additive arm: an unknown token surfaces raw rather than becoming a
    /// second option reading "All" (the old `_ => ALL` fallback).
    #[test]
    fn an_unknown_token_does_not_masquerade_as_all() {
        assert_eq!(type_filter_label("widget"), "widget");
    }
}
