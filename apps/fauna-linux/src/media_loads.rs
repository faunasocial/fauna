//! The per-key image cache every rebuilt card consults — what lets a card
//! rebuild without re-downloading, re-opening and re-decoding the images it
//! already painted, or re-asking its `c2pa-badge` verdict.
//!
//! **Why linux needs one.** A feed card is rebuilt from scratch on every
//! snapshot notification (`views::feed::post_list::render_posts`), and the feed
//! notifies while its embeds resolve (`docs/goal/architecture/apps/linux.md`
//! § Message Flow: a repaint re-reads the whole snapshot). A card that fetched
//! its own images therefore cost a `GET /api/v1/blob/<hash>`, an open, a
//! main-thread `gdk::Texture` decode and a C2PA check per repaint — N of each
//! for a feed of N image posts, however little had changed. With this cache a
//! repaint of an already-painted feed issues none of them. A conversations
//! bubble is rebuilt less often — when its message changes (a reaction, a
//! resolved preview, the reveal, a selection) and on every visit to its
//! thread — but each of those rebuilds re-fetched its link-preview og:image
//! and re-contacted the third-party host of every revealed remote image, so
//! the bubble reads the same cache (tui's conversations page keeps the same
//! session-long remote-image cache).
//!
//! **What is cached, keyed by what.** Four independent caches, never merged:
//! the feed's `post-image` texture and `c2pa-badge` verdict per blob hash, and
//! — on both pages — the link-preview og:image texture per blob hash and the
//! revealed remote image per **url**. The two image kinds a document carries
//! stay apart for tui's reason: a hash names bytes that never change, a url
//! names bytes that can. The DM attachment image is deliberately NOT cached: it
//! fetches nothing (its bytes are already held by the conversations manager)
//! and is decoded only when its bubble is rebuilt, which is bounded by content
//! changes, never by a notification storm.
//!
//! **What is shared and what is not.** The load-state bookkeeping — which key
//! is loading, loaded or hopeless, and that a transient failure is forgotten
//! rather than recorded — is [`fauna_core::load_cache::LoadCache`], the same
//! type tui's immediate-mode pages read every frame. What this module adds is
//! the one thing a *retained-mode* toolkit needs on top: a card built while its
//! key is still loading is not the card that started the load (that one was
//! destroyed by the rebuild), so each asking widget queues a weak reference and
//! is painted when the load lands ([`Loads`]). The fetch stays `FaunaClient`'s
//! (or the shared `fauna_client::remote_image`'s) and the payload stays GTK's —
//! the decoded texture itself, so a gated post's OPENED image survives the
//! rebuild and the lightbox keeps reusing it rather than re-fetching
//! ciphertext.
//!
//! **Scope: one reader's session.** An entry can be a gated post's image opened
//! under a key only the signed-in reader holds, or a remote image only this
//! reader revealed, so every cache belongs to the signed-in reader and to
//! nothing longer-lived. The feed's caches are owned by the current
//! [`LinuxFeedManager`] — rebuilt on every sign-in — so a different manager
//! finds an empty cache and a load that lands after its manager was replaced is
//! discarded. The conversations page has no per-reader manager (its manager is
//! a process singleton), so its cache is owned by the teardown *epoch*: the
//! actor-change teardown (`crate::actor_scope::reset_actor_scoped_state`) drops
//! every cache and advances the epoch, and a load started under an earlier
//! epoch is discarded the same way.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::{Arc, Weak};

use fauna_core::load_cache::{Finished, LoadCache, LoadState};
use gtk::glib;
use gtk::prelude::*;

use crate::feed::host::LinuxFeedManager;

/// What asking for a key told the caller to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Ask<T> {
    /// Already loaded — paint it now.
    Ready(T),
    /// Loaded and hopeless — paint the placeholder, never retry.
    Failed,
    /// Somebody else's load is in flight; the waiter will be handed the result.
    Waiting,
    /// Nobody has asked before (or the last attempt failed transiently): the
    /// caller must start the one load, whose result [`Loads::finish`] then
    /// hands to every waiter, this one included.
    Start,
}

/// [`LoadCache`] plus the retained-mode half: the waiters queued behind each
/// in-flight load. Generic over the payload `T` and the waiter `W` so the
/// bookkeeping is pinned headlessly; in the app `T` is a texture or a verdict
/// and `W` a weak widget reference.
pub struct Loads<T, W> {
    cache: LoadCache<T>,
    waiters: HashMap<String, Vec<W>>,
}

impl<T, W> Default for Loads<T, W> {
    fn default() -> Self {
        Self {
            cache: LoadCache::new(),
            waiters: HashMap::new(),
        }
    }
}

impl<T: Clone, W> Loads<T, W> {
    /// Ask for `key` on behalf of `waiter`. `Waiting` and `Start` both queue the
    /// waiter; `Ready` and `Failed` answer at once and keep nothing.
    pub fn ask(&mut self, key: &str, waiter: W) -> Ask<T> {
        match self.cache.get(key) {
            Some(LoadState::Ready(value)) => return Ask::Ready(value.clone()),
            Some(LoadState::Failed) => return Ask::Failed,
            Some(LoadState::Loading) | None => {}
        }
        let start = self.cache.begin(key);
        self.waiters
            .entry(key.to_string())
            .or_default()
            .push(waiter);
        if start { Ask::Start } else { Ask::Waiting }
    }

    /// Fold a finished load and hand back every waiter queued for it. A
    /// [`Finished::Failed`] is terminal; a [`Finished::Transient`] leaves no
    /// entry, so the next ask starts again. The caller paints the waiters
    /// **after** this returns: a paint can re-enter the widget tree, which must
    /// not find the cache borrowed.
    pub fn finish(&mut self, key: &str, outcome: Finished<T>) -> Vec<W> {
        self.cache.finish(key.to_string(), outcome);
        self.waiters.remove(key).unwrap_or_default()
    }
}

/// A picture cache: a decoded texture per key, waiters are pictures.
type Pictures = Loads<gtk::gdk::Texture, glib::WeakRef<gtk::Picture>>;

/// Which of a rendered document's two fetched image kinds a key names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocImage {
    /// A link-preview og:image, keyed by its nest blob hash.
    Preview,
    /// A revealed third-party remote image, keyed by its url.
    Remote,
}

/// The two document-image caches one page's session holds.
#[derive(Default)]
struct DocImages {
    previews: Pictures,
    remote: Pictures,
}

impl DocImages {
    fn of(&mut self, kind: DocImage) -> &mut Pictures {
        match kind {
            DocImage::Preview => &mut self.previews,
            DocImage::Remote => &mut self.remote,
        }
    }
}

/// Whose cache a build reads — handed to the document walk by the page that
/// builds it.
#[derive(Clone)]
pub enum MediaScope {
    /// A feed page (the list card, `feed.post_detail`): the signed-in reader's
    /// feed manager owns the cache.
    Feed(Arc<LinuxFeedManager>),
    /// The conversations page: the signed-in reader as of this teardown epoch.
    Conversations(u64),
}

impl MediaScope {
    /// The conversations page's scope for a build happening now.
    pub fn conversations() -> Self {
        Self::Conversations(EPOCH.get())
    }
}

/// One reader's feed-page cache, owned by the manager it was built under.
struct FeedSession {
    owner: Weak<LinuxFeedManager>,
    /// The opened, decoded `post-image` texture per blob hash.
    images: Pictures,
    /// The `c2pa-badge` verdict per blob hash — `true` shows the badge.
    c2pa: Loads<bool, glib::WeakRef<gtk::Label>>,
    /// The feed body's og:images and revealed remote images.
    doc: DocImages,
}

/// One reader's conversations-page cache, owned by the epoch it was built in.
struct ConversationsSession {
    epoch: u64,
    doc: DocImages,
}

thread_local! {
    /// The current reader's feed cache. A cell on the GTK thread, like
    /// `crate::screen_lock`'s: the card builders that consult it are handed
    /// the manager, not a page-state struct to hang it off.
    static FEED: RefCell<Option<FeedSession>> = const { RefCell::new(None) };
    /// The current reader's conversations cache.
    static CONVERSATIONS: RefCell<Option<ConversationsSession>> = const { RefCell::new(None) };
    /// The teardown epoch — advanced by every [`clear_for_identity_change`].
    static EPOCH: Cell<u64> = const { Cell::new(0) };
}

/// Run `f` over `manager`'s cache, starting an empty one if the cell holds
/// another manager's (or none) — the builder side.
fn with_feed<R>(manager: &Arc<LinuxFeedManager>, f: impl FnOnce(&mut FeedSession) -> R) -> R {
    FEED.with_borrow_mut(|slot| {
        let current = slot
            .as_ref()
            .is_some_and(|s| s.owner.ptr_eq(&Arc::downgrade(manager)));
        if !current {
            *slot = Some(FeedSession {
                owner: Arc::downgrade(manager),
                images: Loads::default(),
                c2pa: Loads::default(),
                doc: DocImages::default(),
            });
        }
        f(slot.as_mut().expect("just filled"))
    })
}

/// Run `f` over `manager`'s cache only if it is still the current one — the
/// completion side. A load that lands after its reader signed out is dropped,
/// never folded into the next reader's cache.
fn with_current_feed<R>(
    manager: &Arc<LinuxFeedManager>,
    f: impl FnOnce(&mut FeedSession) -> R,
) -> Option<R> {
    FEED.with_borrow_mut(|slot| {
        slot.as_mut()
            .filter(|s| s.owner.ptr_eq(&Arc::downgrade(manager)))
            .map(f)
    })
}

/// The builder side of a document-image ask, for either page.
fn with_doc<R>(scope: &MediaScope, f: impl FnOnce(&mut DocImages) -> R) -> R {
    match scope {
        MediaScope::Feed(manager) => with_feed(manager, |s| f(&mut s.doc)),
        MediaScope::Conversations(epoch) if *epoch == EPOCH.get() => {
            CONVERSATIONS.with_borrow_mut(|slot| {
                if slot.as_ref().is_none_or(|s| s.epoch != *epoch) {
                    *slot = Some(ConversationsSession {
                        epoch: *epoch,
                        doc: DocImages::default(),
                    });
                }
                f(&mut slot.as_mut().expect("just filled").doc)
            })
        }
        // A build under a scope an actor change has already retired: it may
        // still load, but into a cache nobody keeps.
        MediaScope::Conversations(_) => f(&mut DocImages::default()),
    }
}

/// The completion side of a document-image load: `None` when its reader left.
fn with_current_doc<R>(scope: &MediaScope, f: impl FnOnce(&mut DocImages) -> R) -> Option<R> {
    match scope {
        MediaScope::Feed(manager) => with_current_feed(manager, |s| f(&mut s.doc)),
        MediaScope::Conversations(epoch) => CONVERSATIONS.with_borrow_mut(|slot| {
            slot.as_mut()
                .filter(|s| s.epoch == *epoch)
                .map(|s| f(&mut s.doc))
        }),
    }
}

/// Paint every still-alive picture in `waiters` with `outcome`'s texture, if
/// it loaded.
fn paint(waiters: Vec<glib::WeakRef<gtk::Picture>>, outcome: &Finished<gtk::gdk::Texture>) {
    let Finished::Loaded(texture) = outcome else {
        return;
    };
    for picture in waiters.iter().filter_map(glib::WeakRef::upgrade) {
        picture.set_paintable(Some(texture));
    }
}

/// Ask for `hash`'s `post-image` texture on behalf of `picture`.
pub fn ask_image(
    manager: &Arc<LinuxFeedManager>,
    hash: &str,
    picture: &gtk::Picture,
) -> Ask<gtk::gdk::Texture> {
    with_feed(manager, |s| s.images.ask(hash, picture.downgrade()))
}

/// Fold `hash`'s finished `post-image` load and paint every picture still
/// alive that asked for it.
pub fn finish_image(
    manager: &Arc<LinuxFeedManager>,
    hash: &str,
    outcome: Finished<gtk::gdk::Texture>,
) {
    let Some(waiters) = with_current_feed(manager, |s| s.images.finish(hash, outcome.clone()))
    else {
        return;
    };
    paint(waiters, &outcome);
}

/// Ask for `hash`'s provenance verdict on behalf of `badge`.
pub fn ask_c2pa(manager: &Arc<LinuxFeedManager>, hash: &str, badge: &gtk::Label) -> Ask<bool> {
    with_feed(manager, |s| s.c2pa.ask(hash, badge.downgrade()))
}

/// Fold `hash`'s finished verdict (a failed check shows no badge, the same as
/// `false`) and reveal every badge still alive that asked for it on a `true`.
pub fn finish_c2pa(manager: &Arc<LinuxFeedManager>, hash: &str, outcome: Finished<bool>) {
    let shows = outcome == Finished::Loaded(true);
    let Some(waiters) = with_current_feed(manager, |s| s.c2pa.finish(hash, outcome)) else {
        return;
    };
    if !shows {
        return;
    }
    for badge in waiters.iter().filter_map(glib::WeakRef::upgrade) {
        badge.set_visible(true);
    }
}

/// Ask for a document image — an og:image by hash or a remote image by url —
/// on behalf of `picture`, in `scope`'s cache.
pub fn ask_doc_image(
    scope: &MediaScope,
    kind: DocImage,
    key: &str,
    picture: &gtk::Picture,
) -> Ask<gtk::gdk::Texture> {
    with_doc(scope, |d| d.of(kind).ask(key, picture.downgrade()))
}

/// Fold a finished document-image load and paint every picture still alive
/// that asked for it.
pub fn finish_doc_image(
    scope: &MediaScope,
    kind: DocImage,
    key: &str,
    outcome: Finished<gtk::gdk::Texture>,
) {
    let Some(waiters) = with_current_doc(scope, |d| d.of(kind).finish(key, outcome.clone())) else {
        return;
    };
    paint(waiters, &outcome);
}

/// Drop every cache and retire the epoch — the actor-change teardown's call.
pub fn clear_for_identity_change() {
    FEED.with_borrow_mut(|slot| *slot = None);
    CONVERSATIONS.with_borrow_mut(|slot| *slot = None);
    EPOCH.set(EPOCH.get() + 1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_ask_starts_the_load_and_later_asks_queue_behind_it() {
        let mut loads: Loads<u8, &str> = Loads::default();
        assert_eq!(loads.ask("h", "first card"), Ask::Start);
        assert_eq!(loads.ask("h", "rebuilt card"), Ask::Waiting);
        assert_eq!(
            loads.finish("h", Finished::Loaded(7)),
            ["first card", "rebuilt card"]
        );
    }

    #[test]
    fn a_finished_load_answers_every_later_ask_at_once() {
        let mut loads: Loads<u8, &str> = Loads::default();
        assert_eq!(loads.ask("h", "card"), Ask::Start);
        loads.finish("h", Finished::Loaded(7));
        assert_eq!(loads.ask("h", "rebuilt card"), Ask::Ready(7));
        // Nothing was queued by the ready answer, so a stray second finish (a
        // load that should never run) would hand back no one to paint.
        assert!(loads.finish("h", Finished::Loaded(7)).is_empty());
    }

    #[test]
    fn a_failed_load_is_terminal_and_never_restarts() {
        let mut loads: Loads<u8, &str> = Loads::default();
        assert_eq!(loads.ask("h", "card"), Ask::Start);
        assert_eq!(loads.finish("h", Finished::Failed), ["card"]);
        assert_eq!(loads.ask("h", "rebuilt card"), Ask::Failed);
    }

    /// A transient failure hands its waiters back unpainted and forgets the
    /// attempt, so the next rebuild's ask starts a fresh load.
    #[test]
    fn a_transient_failure_restarts_on_the_next_ask() {
        let mut loads: Loads<u8, &str> = Loads::default();
        assert_eq!(loads.ask("h", "card"), Ask::Start);
        assert_eq!(loads.finish("h", Finished::Transient), ["card"]);
        assert_eq!(loads.ask("h", "rebuilt card"), Ask::Start);
    }

    #[test]
    fn keys_load_independently() {
        let mut loads: Loads<u8, &str> = Loads::default();
        assert_eq!(loads.ask("a", "card a"), Ask::Start);
        assert_eq!(loads.ask("b", "card b"), Ask::Start);
        assert_eq!(loads.finish("a", Finished::Loaded(1)), ["card a"]);
        assert_eq!(loads.ask("b", "rebuilt card b"), Ask::Waiting);
    }
}
