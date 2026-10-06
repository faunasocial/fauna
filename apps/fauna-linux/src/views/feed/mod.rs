mod feed_list;
pub mod post_detail;
mod post_list;

pub use post_list::PostListHandles;
// Shared by the document walker's quoted-post embed (`views::document`) as well
// as the feed post list/detail — the muted "unverified source" badge builder.
pub use post_list::{build_delegated_origin_badge, build_unverified_badge};

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;

use crate::client::FaunaClient;
use crate::feed::host::LinuxFeedManager;
use crate::feed::observer;
use fauna_feed::{BridgeFeedView, FeedSummaryView};

/// Handles for the feed view returned to `app.rs`.
#[allow(dead_code)]
pub struct FeedViewHandles {
    /// The post-list pane's content stack — `app.rs` shows the Bluesky thread
    /// view inside it (`BlueskyThreadLoaded`); everything else is self-managed
    /// by the observer loop.
    pub content_stack: gtk::Stack,
    /// The NavigationSplitView itself.
    pub split: adw::NavigationSplitView,
    /// Opens the Feed post-detail pane for a post named only by id — the
    /// `search-result-item` deep-link door (`ui/search.md` § Where logic lives
    /// → Result navigation (deep link)). Resolves via `FeedManager::resolve_post`
    /// first, so a post the timeline never loaded still renders real content
    /// instead of a blank dialog.
    pub open_post_detail: Rc<dyn Fn(String)>,
}

/// Build the full feed view: a navigation split with the feed selector on the
/// left and the post list on the right, rendered entirely from the shared
/// `FeedManager` snapshot via a `FeedSnapshotObserver`. The manager must already be
/// initialised (`crate::feed::host::init`) — `build_main_window` does that right
/// before calling this.
pub fn build_feed_view(client: &Rc<FaunaClient>) -> (adw::NavigationSplitView, FeedViewHandles) {
    let manager = crate::feed::host::manager()
        .expect("feed manager must be initialised before build_feed_view");
    let rt = client.runtime_handle();

    let (list_widget, feed_list_handles) =
        feed_list::build_feed_list_with_boxes(&manager, client, &rt);
    let (post_widget, post_list_handles) = post_list::build_post_list_wired(&manager, client, &rt);

    let list_page = adw::NavigationPage::builder()
        .title(crate::i18n::strings::common::FEED)
        .child(&list_widget)
        .build();
    let detail_page = adw::NavigationPage::builder()
        .title(crate::i18n::strings::common::POSTS)
        .child(&post_widget)
        .build();

    let split = adw::NavigationSplitView::new();
    split.set_sidebar(Some(&list_page));
    split.set_content(Some(&detail_page));

    let trending_list = feed_list_handles.trending_list.clone();
    let feeds_list = feed_list_handles.feeds_list.clone();
    let bridge_list = feed_list_handles.bridge_list.clone();
    let subscribe_btn = feed_list_handles.subscribe_btn.clone();
    let content_stack = post_list_handles.content_stack.clone();
    // Built from a BORROW of `post_list_handles` (not yet moved into the
    // observer loop below) — the deep-link door search wires into its
    // `search-result-item` activation.
    let open_post_detail =
        post_list::open_post_detail_fn(&post_list_handles, &manager, client, &rt);

    // The two selector lists (Trending + the user's own feeds) share one logical
    // selection: picking one clears the other's highlight. `unselect_all()`
    // emits `row-selected(None)` **synchronously**, which the handlers below
    // would otherwise read as "re-query the local feed" — so a shared suppress
    // flag, set around every *programmatic* clear, makes the cleared list's
    // handler a no-op. GTK signals are all on the main thread, so `Cell` suffices.
    let suppress = Rc::new(Cell::new(false));

    // Trending (`feed-trending-item`) → the built-in virtual feed.
    {
        let m = Arc::clone(&manager);
        let rt = rt.clone();
        let feeds_list = feeds_list.clone();
        let suppress = Rc::clone(&suppress);
        trending_list.connect_row_selected(move |_, row| {
            if suppress.get() || row.is_none() {
                return; // programmatic clear, or a deselect (feeds_list drives local)
            }
            suppress.set(true);
            feeds_list.unselect_all();
            suppress.set(false);
            let m = Arc::clone(&m);
            rt.spawn(async move { m.select_trending_feed().await });
        });
        trending_list.connect_row_activated(|list, row| {
            if !row.is_selected() {
                list.select_row(Some(row));
            }
        });
    }

    // Feed-item selection → re-query that feed; no row selected ⇒ the nest's
    // local feed. The row index maps 1:1 into `snapshot.feeds` (`render_feeds`
    // appends in order) — the post-card idiom. It CANNOT ride `widget_name`:
    // `set_test_id(row, "feed-item")` owns that (the automation finder matches
    // widget_name only), and the old widget-name read silently selected the
    // literal feed id "feed-item" on every click, erroring the re-query.
    {
        let m = Arc::clone(&manager);
        let rt = rt.clone();
        let trending_list = trending_list.clone();
        let suppress = Rc::clone(&suppress);
        feeds_list.connect_row_selected(move |_, row| {
            if suppress.get() {
                return; // programmatic clear (Trending was picked)
            }
            let feed_id = row.and_then(|r| {
                let idx = usize::try_from(r.index()).ok()?;
                m.snapshot().feeds.get(idx).map(|f| f.feed_id.clone())
            });
            // A `None` here is a deselect — including the feed-list rebuild on a
            // feeds change (`render_feeds` clears the list). Don't stomp an
            // active Trending selection back to the local feed.
            if feed_id.is_none() && m.snapshot().trending_selected {
                return;
            }
            // Picking a real custom feed clears the Trending row's highlight.
            if row.is_some() {
                suppress.set(true);
                trending_list.unselect_all();
                suppress.set(false);
            }
            let m = Arc::clone(&m);
            rt.spawn(async move { m.select_feed(feed_id).await });
        });
        // The automation agent's ListBoxRow "click" emits only
        // `row-activated` (a pointer click selects first, so this is a no-op
        // for real users): route activation into selection so both input
        // paths hit the single `row_selected` re-query above exactly once.
        feeds_list.connect_row_activated(|list, row| {
            if !row.is_selected() {
                list.select_row(Some(row));
            }
        });
    }

    // ── Observer → main-thread refresh loop ──────────────────────────────
    // The feeds/bridge lists are only re-rendered when they actually change
    // (cheap PartialEq), so a frequent post-only refresh never rebuilds the
    // selector rows (which would fire `row_selected(None)` and reset the
    // selection on every tick).
    {
        let manager_loop = Arc::clone(&manager);
        let client_loop = Rc::clone(client);
        let rt_loop = rt.clone();
        let feeds_list = feeds_list.clone();
        let bridge_list = bridge_list.clone();
        let subscribe_btn = subscribe_btn.clone();
        let feeds_cache: Rc<RefCell<Vec<FeedSummaryView>>> = Rc::new(RefCell::new(Vec::new()));
        let bridge_cache: Rc<RefCell<Vec<BridgeFeedView>>> = Rc::new(RefCell::new(Vec::new()));
        let rx = observer::attach(&manager);
        let content_stack_loop = post_list_handles.content_stack.clone();
        let render = move || {
            refresh(
                &manager_loop,
                &post_list_handles,
                &feeds_list,
                &bridge_list,
                &subscribe_btn,
                &client_loop,
                &rt_loop,
                &feeds_cache,
                &bridge_cache,
            );
        };
        // Initial render so the empty state shows before the first mutation.
        render();
        let mut was_rooted = false;
        crate::async_helper::spawn_wake_loop(rx, move || {
            // Self-terminate once this view's window is torn down, so a
            // re-auth's fresh observer loop doesn't race a dead one.
            if content_stack_loop.root().is_some() {
                was_rooted = true;
            } else if was_rooted {
                return glib::ControlFlow::Break;
            }
            render();
            glib::ControlFlow::Continue
        });
    }

    // Refresh when the feed becomes visible — re-list feeds + bridge feeds and
    // reload the current selection (the feed has no poll backstop, so a nav
    // back must re-pull).
    {
        let m = Arc::clone(&manager);
        let rt = rt.clone();
        split.connect_map(move |_| {
            let m = Arc::clone(&m);
            rt.spawn(async move {
                m.refresh_feeds().await;
                m.refresh_bridge_feeds().await;
                m.refresh_available_bridges().await;
                // Re-pull the current selection — preserving Trending, which is
                // not a `selected_feed` id (trending.md § The Trending feed).
                // The shared seam owns that branch, so every re-pull site reads
                // the same one line instead of re-deriving it.
                m.refresh_current_feed().await;
            });
        });
    }

    let handles = FeedViewHandles {
        content_stack,
        split: split.clone(),
        open_post_detail,
    };
    (split, handles)
}

/// Re-read the snapshot and re-render every pane. The selector lists rebuild
/// only when changed (so a post-only refresh leaves the selection intact).
#[allow(clippy::too_many_arguments)]
fn refresh(
    manager: &Arc<LinuxFeedManager>,
    post_handles: &PostListHandles,
    feeds_list: &gtk::ListBox,
    bridge_list: &gtk::ListBox,
    subscribe_btn: &gtk::Button,
    client: &Rc<FaunaClient>,
    rt: &tokio::runtime::Handle,
    feeds_cache: &RefCell<Vec<FeedSummaryView>>,
    bridge_cache: &RefCell<Vec<BridgeFeedView>>,
) {
    let snap = manager.snapshot();
    // Show the subscribe affordance only when the nest supports at least one
    // bridge (Dim 3 gating — never offer a feed-subscribe for a protocol the
    // nest can't serve).
    subscribe_btn.set_visible(!snap.available_bridges.is_empty());
    if *feeds_cache.borrow() != snap.feeds {
        feed_list::render_feeds(feeds_list, &snap.feeds, manager, rt);
        *feeds_cache.borrow_mut() = snap.feeds.clone();
    }
    if *bridge_cache.borrow() != snap.bridge_feeds {
        feed_list::render_bridge_feeds(bridge_list, &snap.bridge_feeds, manager, rt);
        *bridge_cache.borrow_mut() = snap.bridge_feeds.clone();
    }
    post_list::render_posts(post_handles, &snap, manager, client, rt);
}

/// First `gtk::Label` in a widget subtree (depth-first), so a test can read
/// the count label nested inside an interaction button's or engagement
/// indicator's icon+count box. Shared by `post_list`'s and `post_detail`'s own
/// tests, which each hand-copied this walk under their own private
/// `first_label` before this lift.
#[cfg(test)]
pub(super) fn first_label(root: &impl gtk::prelude::IsA<gtk::Widget>) -> Option<gtk::Label> {
    fn walk(w: &gtk::Widget) -> Option<gtk::Label> {
        if let Some(l) = w.downcast_ref::<gtk::Label>() {
            return Some(l.clone());
        }
        let mut c = w.first_child();
        while let Some(child) = c {
            if let Some(found) = walk(&child) {
                return Some(found);
            }
            c = child.next_sibling();
        }
        None
    }
    walk(root.upcast_ref())
}
