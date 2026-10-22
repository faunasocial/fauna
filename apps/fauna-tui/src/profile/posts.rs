//! The profile page's Posts tab (`profile.md` § Layout & flow → *The Posts
//! tab*): the viewed actor's posts, newest first, as the feed paints them.
//!
//! The tab adds no post logic of its own. Its rows are an **author window** of
//! the feed manager — `FeedManager::author_window(actor_id)`, minted from the
//! timeline's manager on every open of the tab (no client cache — `profile.md`
//! § Persistence), reading `fauna.feed.author.posts` (`feed.md` § State & data
//! shape → *The author window*) — and they paint through the feed page's own
//! card walk, [`crate::feed::post_card_rows`]. Loading, empty and error come off
//! the window's `FeedSnapshot` exactly as on the feed: the shared
//! `FeedSnapshot::empty_state()` decides `profile-posts-empty`, and the
//! snapshot's error lands on the page's `error-message` ([`page_error`]).
//!
//! **The cards' gestures.** The walk emits `Gesture::Feed`; here every one is
//! re-routed through [`Action::Post`] to [`route`], which sends each verb to the
//! manager its row came from:
//! - the card's activation is the door a search result uses — the timeline
//!   manager's `resolve_post` and the feed page's `post_detail`
//!   (`feed.md` § The read model → *Opening a post the timeline never loaded*);
//! - the window's own verbs (like / repost / quote, the buy, the remote-image
//!   reveal) act on the window, so the counts it paints move;
//! - the render-decision reveals and the lightbox are the feed state's own
//!   keyed sets, which the shared walk reads on both pages;
//! - the card's ⋯ menu, reply dialog and tip list open here, painted by the
//!   feed's own overlay painters over the window ([`feed::post_overlays`]), and
//!   their verbs (train, web publish, delete, the reply's submit) resolve their
//!   post against the window ([`feed::apply_on`]).

use std::sync::Arc;

use fauna_i18n::strings::profile as t;
use fauna_ui_ids as ids;

use super::{Action, Op, Tab};
use crate::app::App;
use crate::element::{Element, Gesture, Role};
use crate::feed::{self, CliFeedManager};
use crate::pages::Page;

/// Mint the tab's author window for the actor on screen and hand back the op
/// that reads its first page. Called on every open of the tab — the page's open
/// ([`App::open_profile`]) and the `profile-posts-tab` gesture — so each open
/// reads afresh. `None` before a session (no timeline manager to mint from).
pub(crate) fn open(app: &mut App) -> Option<Op> {
    let timeline = app.feed.manager.as_ref()?;
    let window = Arc::new(timeline.author_window(app.profile.actor_id().to_string()));
    feed::observe(&window, &app.tx);
    app.profile.posts = Some(Arc::clone(&window));
    Some(Op::Feed(Box::new(feed::Op::RefreshCurrentFeed {
        manager: window,
    })))
}

/// The next page of the window — the feed's load-more gesture (the focus ring
/// arriving on the last card, [`at_last_post`]).
pub(super) fn load_more(app: &App) -> Option<Op> {
    let manager = app.profile.posts.clone()?;
    Some(Op::Feed(Box::new(feed::Op::LoadMore { manager })))
}

/// Whether the focus ring sits on the tab's last card while the window holds
/// more — `App::handle_key`'s load-more edge for this page.
pub fn at_last_post(app: &App) -> bool {
    app.page == Page::Profile
        && app.profile.tab == Tab::Posts
        && app
            .profile
            .posts
            .as_ref()
            .is_some_and(|w| feed::focus_at_last_card(app, &w.snapshot()))
}

/// The window's read failure, for the page's `error-message` while the tab is
/// showing (`profile.md` § The Posts tab → *Empty, loading, error*).
pub(crate) fn page_error(app: &App) -> Option<String> {
    if app.profile.tab != Tab::Posts {
        return None;
    }
    let snapshot = app.profile.posts.as_ref()?.snapshot();
    snapshot
        .error
        .map(|e| e.resolve(fauna_i18n::strings::lookup))
}

/// The tab's content: `profile-posts-section`, then `profile-posts-empty` or the
/// window's cards, newest first. Nothing below the section while the first
/// read is in flight (the shared empty state is absent until the window is
/// `Loaded`).
pub(super) fn elements(app: &App, out: &mut Vec<Element>) {
    out.push(Element::label(ids::PROFILE_POSTS_SECTION, " "));
    let Some(window) = app.profile.posts.as_ref() else {
        return;
    };
    let snapshot = window.snapshot();
    // The `feed-empty-state` rule through the same shared decision; the window
    // carries no search, so its one empty arm is "no posts".
    if snapshot.empty_state().is_some() {
        out.push(Element::label(ids::PROFILE_POSTS_EMPTY, t::NO_POSTS));
    }
    out.extend(
        feed::post_card_rows(
            app,
            Some(window.as_ref()),
            &snapshot.posts,
            &snapshot.bridge_roster,
        )
        .into_iter()
        .map(routed),
    );
    // The card's ⋯ menu, tip list and reply dialog, flat and once after the
    // cards as on the feed — the same painters, over the window.
    out.extend(
        feed::post_overlays(app, Some(window.as_ref()), &snapshot)
            .into_iter()
            .map(routed),
    );
    // A picture opened from a card enlarges here, as on the feed: the lightbox
    // is the feed state's own overlay, and its close is page-agnostic.
    out.extend(feed::lightbox_elements(&app.feed));
}

/// Re-route a card or overlay element's feed gesture through this page
/// ([`route`]) — a button's, and the reply dialog's public-confirm checkbox's.
fn routed(mut element: Element) -> Element {
    match &mut element.role {
        Role::Button(gesture) | Role::Checkbox { gesture, .. } => {
            if let Gesture::Feed(action) = gesture {
                *gesture = Gesture::Profile(Action::Post(action.clone()));
            }
        }
        _ => {}
    }
    element
}

/// A card gesture on the tab, sent to the manager its row came from (module
/// docs).
pub(super) fn route(app: &mut App, action: feed::Action) -> Option<Op> {
    use feed::Action as F;
    let window: Option<Arc<CliFeedManager>> = app.profile.posts.clone();
    match action {
        F::OpenPostDetail(post_id) => open_detail(app, post_id),
        F::Interact { post_id, action } => window.map(|manager| {
            Op::Feed(Box::new(feed::Op::Interact {
                manager,
                post_id,
                action,
            }))
        }),
        #[cfg(feature = "payments")]
        F::BuyUnlockOffer(post_id) => {
            window.map(|manager| Op::Feed(Box::new(feed::Op::BuyUnlockOffer { manager, post_id })))
        }
        F::RevealRemoteImages(post_id) => {
            window?.reveal_remote_images(post_id);
            None
        }
        reveal @ (F::RevealMuted(_) | F::RevealContent(_) | F::OpenLightbox(_)) => {
            feed::apply_local(app, reveal).map(|op| Op::Feed(Box::new(op)))
        }
        #[cfg(feature = "payments")]
        link @ F::OpenPaymentLink(_) => {
            feed::apply_local(app, link).map(|op| Op::Feed(Box::new(op)))
        }
        // The overlays and every verb they fire, over the window: the
        // feed's own arms, resolving the post against the manager the row
        // came from.
        overlay @ (F::OpenPostActions(_)
        | F::OpenReplyDialog(_)
        | F::ToggleReplyPublicConfirm
        | F::SubmitReply
        | F::TrainPost { .. }
        | F::StartDeletePost(_)
        | F::ConfirmDeletePost(_)
        | F::PublishWeb(_)
        | F::UnpublishWeb(_)
        | F::CopyWebLink(_)
        | F::CopyPaywallLink(_)) => {
            feed::apply_on(app, window?, overlay).map(|op| Op::Feed(Box::new(op)))
        }
        #[cfg(feature = "payments")]
        tips @ F::OpenTipList(_) => {
            feed::apply_on(app, window?, tips).map(|op| Op::Feed(Box::new(op)))
        }
        // The feed-video handoff's confirm lives on the feed page only (its
        // ui.yaml scope), so the tab opens the post's detail there, where the
        // thumbnail runs the handoff (`apps/tui.md` § External media handoff →
        // *Feed video*).
        F::OpenVideo { post_id, .. } => open_detail(app, post_id),
        // The walk emits nothing else.
        _ => None,
    }
}

/// The door a search result uses: the feed page's `post_detail`, the post
/// resolved through the timeline manager's `resolve_post` (`profile.md` § The
/// Posts tab → *What a row is*).
fn open_detail(app: &mut App, post_id: String) -> Option<Op> {
    app.page = Page::Feed;
    feed::apply_local(app, feed::Action::OpenPostDetail(post_id)).map(|op| Op::Feed(Box::new(op)))
}
