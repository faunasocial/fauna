//! The "Muted words" Settings sub-page — the tier-1 user keyword filter
//! (`docs/goal/behavior/moderation.md` § Muted keywords;
//! `docs/goal/architecture/content-moderation-and-ranking.md` § Resolved
//! design decisions Q3). linux is the reference leg (ui.yaml `muted-words`
//! page, reached `{"view":"settings","id":"muted-words"}`, placed after
//! Privacy in the rail — `docs/goal/ui/settings.md` § Navigation model); the
//! other five apps lift this shape (priority #1).
//!
//! A person manages their single user-global muted-keyword list here: add a
//! term (`muted-word-input` + `muted-word-add-button`), see the terms
//! (`muted-word-item` rows, indexed: `muted-word-text` +
//! `muted-word-remove-button`), empty state (`muted-word-empty`).
//!
//! Unlike the mail-aliases/mail-lists sub-pages (which front a per-feature
//! `*Machine`), the muted-keywords seam needs no state machine:
//! the shared
//! `fauna_sync_engine::preference_surfaces::{load_muted_words,add_muted_word,
//! remove_muted_word}` own the whole round trip (reaching the account store,
//! the read, normalize-on-write — trim, drop blanks, case-insensitive dedupe) and
//! hand back the page record `MutedWordsSnapshot`, so no machine abstraction
//! earns its keep and this page just dispatches those calls, mirroring `views/backups/destinations.rs`'s
//! direct-shared-seam shape (priority #2 — no redundant layer for a seam this
//! thin).
//!
//! **`muted-word-empty` is gated on the record's `loaded` bit, not on row
//! count** (`docs/goal/ui/README.md` § *List pages: loading is not empty*): the
//! label is built hidden and only a resolved read may show it, because an empty
//! list means "no terms" only once something has actually read one.
//!
//! **Every load AND save updates the `crate::conversations` thread-local
//! muted-keywords cache** (`set_muted_keywords_cache`) so the conversation
//! bubble collapse (`views/conversations/message_bubble.rs`) reflects the
//! current list immediately — the two surfaces (this CRUD page + the bubble
//! collapse) share one client-side cache; there is no second read path. (The
//! cache is also populated once at auth, `FaunaClient::load_muted_keywords`,
//! so the collapse works on a fresh launch without visiting this page first.)

use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

use fauna_client_config::MutedWordsSnapshot;

use crate::async_helper::{hydrate_with_retry, spawn_with_snapshot};
use crate::client::FaunaClient;
use crate::i18n::strings::muted_words as S;
use crate::testid::set_test_id;

/// One async round-trip's result: the freshly-read/persisted page record (the
/// normalized muted-word list **and** its `loaded` bit), or an error string
/// surfaced in the page `error-message`.
type MutationResult = Result<MutedWordsSnapshot, String>;

/// Widget handles the render + event closures need.
struct Widgets {
    error_label: gtk::Label,
    input: gtk::Entry,
    add_button: gtk::Button,
    /// `muted-word-list` — container the indexed `muted-word-item` rows are
    /// rebuilt into.
    list: gtk::Box,
    empty: gtk::Label,
    rows: RefCell<Vec<gtk::Box>>,
    /// The page record the list currently shows ([`paint`]).
    painted: RefCell<Option<MutedWordsSnapshot>>,
}

/// Everything the handlers + render need.
struct Ctx {
    rt: tokio::runtime::Handle,
    w: Widgets,
}

/// Build the "Muted words" preferences page. Returns the widget plus a
/// refresh closure — the settings shell is build-once (`views::settings_shell`),
/// so without an explicit on-visible re-fetch this page's one `wire`-time load
/// would never see a term added elsewhere: from another device (pre-existing
/// gap), or — since W5.6 (account-data-plane.md § Workstreams) (2026-08-15) — from a **concurrent same-account
/// instance sharing this account's sealed config** (`account-scoping.md` §
/// Concurrent instances; `account-data-plane.md` § Multi-instance concurrency).
/// Mirrors `build_account_page`'s on-visible refresh shape exactly.
pub fn build_muted_words_page(client: &Rc<FaunaClient>) -> (gtk::Box, Rc<dyn Fn()>) {
    let (page, widgets) = build_page_widgets();
    let ctx = wire(client, widgets);
    let refresh_fn: Rc<dyn Fn()> = Rc::new(move || refresh(&ctx));
    (
        crate::testid::wrap_page_with_heading(S::TITLE, ids::MUTED_WORDS, &page),
        refresh_fn,
    )
}

/// Build the static page widget tree (every ui.yaml ID present) with no client
/// dependency — split out so the unit test can exercise ID-conformance without
/// a real `FaunaClient` (mirrors `views/backups/destinations.rs`'s
/// `build_form`/`build_remove_modal` split).
fn build_page_widgets() -> (adw::PreferencesPage, Widgets) {
    let page = adw::PreferencesPage::builder()
        .title(S::TITLE)
        .icon_name("action-unavailable-symbolic")
        .build();

    // --- Top group: heading/description + page-level error + add row ---
    let top_group = adw::PreferencesGroup::builder()
        .title(S::TITLE)
        .description(S::DESCRIPTION)
        .build();

    // error-message — page-level error label (Rule 2), hidden until set.
    let error_label = gtk::Label::builder().visible(false).build();
    error_label.add_css_class("error");
    error_label.set_halign(gtk::Align::Start);
    error_label.set_wrap(true);
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    top_group.add(&error_label);

    let input = gtk::Entry::builder()
        .placeholder_text(S::INPUT_PLACEHOLDER)
        .hexpand(true)
        .build();
    set_test_id(&input, ids::MUTED_WORD_INPUT);

    let add_button = gtk::Button::with_label(S::ADD);
    add_button.add_css_class("suggested-action");
    add_button.set_valign(gtk::Align::Center);
    set_test_id(&add_button, ids::MUTED_WORD_ADD_BUTTON);
    crate::offline_gate::declare_wire_kind(&add_button, "fauna.account.state.put");

    let input_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    input_row.append(&input);
    input_row.append(&add_button);
    top_group.add(&input_row);
    page.add(&top_group);

    // --- Muted-word list group ---
    let list_group = adw::PreferencesGroup::builder().title(S::TITLE).build();

    let list = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .build();
    set_test_id(&list, ids::MUTED_WORD_LIST);
    list_group.add(&list);

    let empty = gtk::Label::new(Some(S::EMPTY));
    empty.add_css_class("dim-label");
    empty.set_halign(gtk::Align::Start);
    // Hidden until a read resolves — a page that has not read yet has no
    // business saying the list is empty (`render_rows` is the only thing that
    // may show it, and it runs only on a successful round trip).
    empty.set_visible(false);
    set_test_id(&empty, ids::MUTED_WORD_EMPTY);
    list_group.add(&empty);
    page.add(&list_group);

    let widgets = Widgets {
        error_label,
        input,
        add_button,
        list,
        empty,
        rows: RefCell::new(Vec::new()),
        painted: RefCell::new(None),
    };
    (page, widgets)
}

/// Wire the page to the shared account-store seam, load on mount, connect
/// every interaction, and hand back the shared `ctx` so the caller can rebuild
/// an on-visible refresh closure over the same widgets.
fn wire(client: &Rc<FaunaClient>, widgets: Widgets) -> Rc<Ctx> {
    let ctx = Rc::new(Ctx {
        rt: client.runtime_handle(),
        w: widgets,
    });

    refresh(&ctx);

    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .add_button
            .clone()
            .connect_clicked(move |_| submit_add(&ctx));
    }
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .input
            .clone()
            .connect_activate(move |_| submit_add(&ctx));
    }

    ctx
}

/// Read the input and dispatch an add, if non-empty.
fn submit_add(ctx: &Rc<Ctx>) {
    let term = ctx.w.input.text().trim().to_string();
    if term.is_empty() {
        return;
    }
    dispatch(ctx, Mutation::Add(term));
}

/// Load the current muted-keyword list off the account store on mount.
fn refresh(ctx: &Rc<Ctx>) {
    let store = crate::account_runtime::handle_source();
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            // Kept: not a single NestClient RPC — a local store read that
            // waits for the account runtime's assembly (transport.md §
            // Request lifecycle step 3's note).
            hydrate_with_retry(|| load_words(store.clone())).await
        },
        move |result| apply_load(&ctx_render, result),
    );
}

/// The two persisting actions the page dispatches.
enum Mutation {
    Add(String),
    Remove(String),
}

/// Run a mutation on the tokio runtime, then apply the result on the GTK thread.
fn dispatch(ctx: &Rc<Ctx>, mutation: Mutation) {
    ctx.w.add_button.set_sensitive(false);
    let store = crate::account_runtime::handle_source();
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            match mutation {
                Mutation::Add(term) => add_word(store, term).await,
                Mutation::Remove(term) => remove_word(store, term).await,
            }
        },
        move |result| apply(&ctx_render, result),
    );
}

/// Render a MUTATION's outcome (GTK main thread): the add button comes back,
/// and on success the draft clears and the rows re-render from the
/// freshly-persisted list. On error the message shows; the rows are untouched.
fn apply(ctx: &Rc<Ctx>, result: MutationResult) {
    ctx.w.add_button.set_sensitive(true);
    match result {
        Ok(snapshot) => {
            super::render_error_label(&ctx.w.error_label, None);
            ctx.w.input.set_text("");
            paint(ctx, &snapshot);
        }
        Err(msg) => super::render_error_label(&ctx.w.error_label, Some(&msg)),
    }
}

/// Render a LOAD's outcome (GTK main thread) — the mount, the on-visible
/// refresh, and the store-change notice's re-drive (`crate::store_surfaces`).
/// The notice is a level, so a load never touches what a gesture owns: the
/// draft in the input stays, and the add button keeps whatever an in-flight
/// mutation set — only that mutation's own outcome ([`apply`]) clears either.
fn apply_load(ctx: &Rc<Ctx>, result: MutationResult) {
    match result {
        Ok(snapshot) => {
            super::render_error_label(&ctx.w.error_label, None);
            paint(ctx, &snapshot);
        }
        Err(msg) => super::render_error_label(&ctx.w.error_label, Some(&msg)),
    }
}

/// Paint the page record and update the `crate::conversations` cache — the
/// load-bearing step that keeps the bubble collapse in sync with every edit.
/// A record equal to the one already painted paints nothing.
fn paint(ctx: &Rc<Ctx>, snapshot: &MutedWordsSnapshot) {
    if ctx.w.painted.borrow().as_ref() == Some(snapshot) {
        return;
    }
    *ctx.w.painted.borrow_mut() = Some(snapshot.clone());
    render_rows(ctx, snapshot);
    crate::conversations::set_muted_keywords_cache(snapshot.keywords.clone());
}

/// Rebuild the `muted-word-item` row list from the persisted page record.
fn render_rows(ctx: &Rc<Ctx>, snapshot: &MutedWordsSnapshot) {
    let mut rows = ctx.w.rows.borrow_mut();
    for row in rows.drain(..) {
        ctx.w.list.remove(&row);
    }
    for word in snapshot.terms() {
        let row = build_word_row(ctx, &word);
        ctx.w.list.append(&row);
        rows.push(row);
    }
    // Two conditions, not one: a read has resolved AND it found nothing —
    // the shared predicate, so this page cannot re-derive it differently.
    ctx.w.empty.set_visible(snapshot.shows_empty_state());
}

/// Build one `muted-word-item` row: the term text + a remove button.
fn build_word_row(ctx: &Rc<Ctx>, word: &str) -> gtk::Box {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&row, ids::MUTED_WORD_ITEM);

    let text = gtk::Label::new(Some(word));
    text.set_hexpand(true);
    text.set_halign(gtk::Align::Start);
    set_test_id(&text, ids::MUTED_WORD_TEXT);
    row.append(&text);

    let remove = gtk::Button::with_label(S::REMOVE);
    remove.add_css_class("flat");
    remove.set_valign(gtk::Align::Center);
    set_test_id(&remove, ids::MUTED_WORD_REMOVE_BUTTON);
    crate::offline_gate::declare_wire_kind(&remove, "fauna.account.state.put");
    {
        let ctx = Rc::clone(ctx);
        let term = word.to_string();
        remove.connect_clicked(move |_| {
            dispatch(&ctx, Mutation::Remove(term.clone()));
        });
    }
    row.append(&remove);

    row
}

// ── Shared-call sequencing (the only logic here; everything is shared Rust) ──
//
// The tokio-runtime side takes only `Send` inputs (the account store's
// source), so the page's `Rc<FaunaClient>` never crosses the spawn boundary.

type Store = fauna_sync_engine::account_runtime::SeatAccountStore;

/// The shared [`fauna_sync_engine::preference_surfaces::load_muted_words`]
/// tui's `settings/muted_words.rs` also calls, its error as
/// the page's string.
async fn load_words(store: Store) -> MutationResult {
    fauna_sync_engine::preference_surfaces::load_muted_words(&store)
        .await
        .map_err(fauna_sync_engine::preference_surfaces::plane_failure)
}

/// Add a term through the shared delta (linux's local
/// re-read-then-replace pattern, lifted so all 7 apps carry it and the re-read
/// happens INSIDE the record update rather than in a separate racy round trip
/// before it): [`fauna_sync_engine::preference_surfaces::add_muted_word`].
async fn add_word(store: Store, term: String) -> MutationResult {
    fauna_sync_engine::preference_surfaces::add_muted_word(&store, &term)
        .await
        .map_err(fauna_sync_engine::preference_surfaces::plane_failure)
}

/// Remove a term — [`add_word`]'s inverse on the same shared surface.
async fn remove_word(store: Store, term: String) -> MutationResult {
    fauna_sync_engine::preference_surfaces::remove_muted_word(&store, &term)
        .await
        .map_err(fauna_sync_engine::preference_surfaces::plane_failure)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::testid::widget_names;

    /// The deterministic half of the loading-is-not-empty rule
    /// (`docs/goal/ui/README.md` § *List pages: loading is not empty*), pinned
    /// here rather than in the e2e because "before the read resolves" is a race
    /// at that level: the freshly-built page — exactly what a user sees between
    /// navigating and the first reply — must not claim the list is empty.
    ///
    /// The id still EXISTS in the tree (the test above asserts that); what this
    /// pins is that it is not *shown*, which is what `is_visible` reads. Deleting
    /// the `set_visible(false)` at build time turns this red.
    #[test]
    fn the_empty_state_is_hidden_until_a_read_resolves() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();

            let (_page, widgets) = build_page_widgets();
            assert!(
                !widgets.empty.is_visible(),
                "a page that has not read yet must not paint muted-word-empty"
            );
        });
    }

    /// The page exposes every static ui.yaml ID with no registered client
    /// (`build_muted_words_page` requires a real `FaunaClient` for `wire`, so
    /// this test exercises the client-free `build_page_widgets` split, the
    /// exact widget tree the real page builds before wiring).
    #[test]
    fn muted_words_page_exposes_static_ui_yaml_ids() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();

            let (page, _widgets) = build_page_widgets();
            let wrapped = crate::testid::wrap_page_with_heading(S::TITLE, ids::MUTED_WORDS, &page);
            let names = widget_names(&wrapped);
            for id in [
                "muted-words",
                "error-message",
                "muted-word-input",
                "muted-word-add-button",
                "muted-word-list",
                "muted-word-empty",
            ] {
                assert!(
                    names.iter().any(|n| n == id),
                    "missing widget id {id:?}; have {names:?}"
                );
            }
        });
    }
}
