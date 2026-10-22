//! The profile page's Posts tab (`profile.md` § Layout & flow → *The Posts
//! tab*): the viewed actor's posts, newest first, as the feed paints them —
//! linux's lift of tui's `apps/fauna-tui/src/profile/posts.rs`.
//!
//! The tab adds no post logic of its own. Its rows are an **author window** of
//! the feed manager — `FeedManager::author_window(actor_id)`, minted from the
//! timeline's manager on every open of the tab (no client cache — `profile.md`
//! § Persistence), reading `fauna.feed.author.posts` (`feed.md` § State & data
//! shape → *The author window*) — and they paint through the feed page's own
//! card builder, [`crate::views::feed::paint_post_cards`], which also fires the
//! cards' lazy resolves on the window. Loading, empty and error come off the
//! window's `FeedSnapshot` exactly as on the feed: the shared
//! `FeedSnapshot::empty_state()` decides `profile-posts-empty`, and the
//! snapshot's error lands on the page's `error-message`.
//!
//! **The cards' gestures.** Every card verb is wired by the feed's builder to
//! the manager it was handed — here the window — so a like / repost / quote,
//! the ⋯ menu's verbs (train, web publish, delete), the reply dialog and the
//! tip list all open on the profile and act on the window. The card's
//! activation is the door a search result uses: the feed page's `post_detail`
//! over the timeline manager's `resolve_post` (`feed.md` § The read model →
//! *Opening a post the timeline never loaded*), handed in by `app.rs` as
//! `open_post`. The load-more gesture is the feed's own (its unnamed "Load
//! more" button).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use fauna_ui_ids as ids;

use crate::client::FaunaClient;
use crate::feed::host::LinuxFeedManager;
use crate::i18n::strings::common;
use crate::i18n::strings::profile as p;
use crate::testid::set_test_id;

/// Build the tab's content host (`profile-posts-section`) and the closure that
/// opens the tab for `actor_id` — minting a fresh author window and reading its
/// first page. `open_post` is the search door into the feed's `post_detail`.
/// Nothing is read until the closure runs: the page calls it on every open of
/// the tab (the page becoming visible, each `profile-posts-tab` activation).
pub(super) fn build(
    client: &Rc<FaunaClient>,
    actor_id: &str,
    error_label: gtk::Label,
    open_post: Rc<dyn Fn(String)>,
) -> (gtk::Box, Rc<dyn Fn()>) {
    let section = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .build();
    set_test_id(&section, ids::PROFILE_POSTS_SECTION);

    // `profile-posts-empty` — iff the window is `Loaded` and holds no post.
    let empty = gtk::Label::new(Some(p::NO_POSTS));
    empty.set_halign(gtk::Align::Start);
    empty.add_css_class("dim-label");
    empty.set_margin_start(12);
    empty.set_visible(false);
    set_test_id(&empty, ids::PROFILE_POSTS_EMPTY);
    section.append(&empty);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.set_activate_on_single_click(true);
    list.add_css_class("boxed-list");
    list.set_margin_start(12);
    list.set_margin_end(12);
    section.append(&list);

    // The feed's load-more gesture, shown while the window holds more.
    let load_more = gtk::Button::with_label(common::LOAD_MORE);
    load_more.set_halign(gtk::Align::Center);
    load_more.set_visible(false);
    section.append(&load_more);

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&section)
        .build();
    let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
    host.append(&scrolled);

    // The window of the tab's current open. Each open replaces it and bumps
    // the generation, which retires the previous open's repaint loop.
    let window: Rc<RefCell<Option<Arc<LinuxFeedManager>>>> = Rc::new(RefCell::new(None));
    let generation = Rc::new(Cell::new(0u64));

    // Card activation → the search door. A REPOST ROW opens its original, as
    // on the feed (`feed.md` § Interaction bar → Repost).
    {
        let window = Rc::clone(&window);
        list.connect_row_activated(move |_, row| {
            let Some(w) = window.borrow().clone() else {
                return;
            };
            let idx = row.index().max(0) as usize;
            if let Some(post) = w.snapshot().posts.get(idx).cloned() {
                open_post(post.reposted_post_id.unwrap_or(post.post_id));
            }
        });
    }
    {
        let window = Rc::clone(&window);
        let rt = client.runtime_handle();
        load_more.connect_clicked(move |_| {
            if let Some(w) = window.borrow().clone() {
                rt.spawn(async move { w.load_more().await });
            }
        });
    }

    let open: Rc<dyn Fn()> = {
        let client = Rc::clone(client);
        let actor_id = actor_id.to_string();
        let section = section.clone();
        Rc::new(move || {
            // No session yet → no timeline manager to mint from.
            let Some(timeline) = crate::feed::host::manager() else {
                return;
            };
            let w = Arc::new(timeline.author_window(actor_id.clone()));
            *window.borrow_mut() = Some(Arc::clone(&w));
            let this_open = generation.get() + 1;
            generation.set(this_open);

            let render = {
                let w = Arc::clone(&w);
                let client = Rc::clone(&client);
                let list = list.clone();
                let empty = empty.clone();
                let load_more = load_more.clone();
                let error_label = error_label.clone();
                let showed_error = Rc::new(Cell::new(false));
                move || {
                    paint(
                        &w,
                        &client,
                        &list,
                        &empty,
                        &load_more,
                        &error_label,
                        &showed_error,
                    )
                }
            };
            render();

            let rx = crate::feed::observer::attach(&w);
            let built_for = crate::actor_scope::current_generation();
            let generation = Rc::clone(&generation);
            let section = section.clone();
            let mut was_rooted = false;
            crate::async_helper::spawn_wake_loop(rx, move || {
                // Retire on a newer open of the tab, an actor change, or the
                // page being torn down (`open_profile` rebuilds it per target).
                if generation.get() != this_open
                    || !crate::actor_scope::poll_still_current(built_for)
                {
                    return glib::ControlFlow::Break;
                }
                if section.root().is_some() {
                    was_rooted = true;
                } else if was_rooted {
                    return glib::ControlFlow::Break;
                }
                render();
                glib::ControlFlow::Continue
            });

            client
                .runtime_handle()
                .spawn(async move { w.refresh_current_feed().await });
        })
    };

    (host, open)
}

/// Repaint the tab from the window's snapshot (module docs).
fn paint(
    window: &Arc<LinuxFeedManager>,
    client: &Rc<FaunaClient>,
    list: &gtk::ListBox,
    empty: &gtk::Label,
    load_more: &gtk::Button,
    error_label: &gtk::Label,
    showed_error: &Cell<bool>,
) {
    let snap = window.snapshot();
    // The `feed-empty-state` rule through the same shared decision; the window
    // carries no search, so its one empty arm is "no posts".
    empty.set_visible(snap.empty_state().is_some());
    load_more.set_visible(snap.has_more && !snap.posts.is_empty());
    // The read's failure on the page's `error-message`; cleared only if this
    // tab put it there (the label is shared with the rest of the page).
    match &snap.error {
        Some(lt) => {
            error_label.set_text(&lt.resolve(crate::i18n::strings::lookup));
            error_label.set_visible(true);
            showed_error.set(true);
        }
        None if showed_error.replace(false) => error_label.set_visible(false),
        None => {}
    }
    crate::views::feed::paint_post_cards(
        list,
        &snap,
        window,
        client,
        &client.runtime_handle(),
        error_label,
    );
}
