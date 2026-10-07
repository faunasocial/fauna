use adw::prelude::*;
use fauna_ui_ids as ids;
use gtk::glib;
use gtk::prelude::FileExt;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use tokio::runtime::Handle;

use crate::client::FaunaClient;
use crate::feed::host::LinuxFeedManager;
use crate::media_loads::{self, Ask};
use crate::nest_content_api::ApiError;
use fauna_client::NestClient;
use fauna_core::load_cache::Finished;

use super::post_detail::build_post_detail;
#[cfg(feature = "payments")]
use crate::i18n::strings::tips;
use crate::i18n::strings::{c2pa, common, composer, family, feed, subscriptions, web_publish};
use crate::views::document;
use fauna_core::obligation::RenderVerdict;
use fauna_core::render::{AuthoringOriginStatus, VerificationStatus};
#[cfg(feature = "payments")]
use fauna_feed::TipView;
use fauna_feed::{
    AttachedFile, FeedEmptyState, FeedSnapshot, FeedStatus, PostSummary, SellComposeState,
    TrainVerb, classify_sources,
};

/// The open detail's last-painted embed signature — see
/// [`PostListHandles::open_detail_sig`] for the five booleans' meaning.
type OpenDetailSig = Rc<Cell<(bool, bool, bool, bool, bool)>>;

/// The widgets and change-tracking handles [`build_compose_post_wired`] wires
/// up: the compose box, its error label, the gate-tier select, the select's
/// current option model, the body/tags inputs, the programmatic-update guard,
/// and the file-ready chip + its remove control.
type ComposePostWidgets = (
    gtk::Box,
    gtk::Label,
    AudienceControls,
    gtk::TextView,
    gtk::Entry,
    Rc<Cell<bool>>,
    gtk::Label,
    gtk::Button,
);

/// The composer's audience select (`compose-gate-tier-select`) as a model:
/// Public, the author's own tiers, one option per room they can address a
/// post to ("Room: ‹label›", `snapshot.own_rooms`), then "Sell this post…"
/// last — tui's order (`ui/feed.md` § Encryption at rest → *Room-restricted —
/// the app half*).
///
/// An answer is read off an option's **position**, never its text: `tiers.create`
/// refuses only the reserved `room` name, so an author can still name a tier
/// exactly a room option's or the Sell option's label, and a text compare would
/// let that tier hijack the other answer.
#[derive(Default, PartialEq)]
struct GateOptions {
    tiers: Vec<String>,
    rooms: Vec<fauna_feed::GateRoomOption>,
}

/// One answer to the audience select — mutually exclusive by construction.
#[derive(Debug, PartialEq)]
enum GateAnswer {
    Public,
    Tier(String),
    /// A room by its hex channel id.
    Room(String),
    Sell,
}

impl GateOptions {
    /// The option labels, in select order.
    fn labels(&self) -> Vec<String> {
        let mut labels = vec![feed::post::GATE_PUBLIC.to_string()];
        labels.extend(self.tiers.iter().cloned());
        labels.extend(self.rooms.iter().map(|r| feed::post::gate_room(&r.label)));
        labels.push(feed::post::GATE_SELL.to_string());
        labels
    }

    /// The answer the option at `idx` gives (`INVALID_LIST_POSITION` ⇒ Public).
    fn answer(&self, idx: u32) -> GateAnswer {
        if idx == 0 || idx == gtk::INVALID_LIST_POSITION {
            return GateAnswer::Public;
        }
        let i = idx as usize - 1;
        if let Some(tier) = self.tiers.get(i) {
            return GateAnswer::Tier(tier.clone());
        }
        let i = i - self.tiers.len();
        if let Some(room) = self.rooms.get(i) {
            return GateAnswer::Room(room.room.clone());
        }
        if i == self.rooms.len() {
            GateAnswer::Sell
        } else {
            GateAnswer::Public
        }
    }

    /// Where `answer` sits in this model — so a rebuilt option list keeps the
    /// user's in-progress choice rather than snapping back to Public. An
    /// answer the new list no longer offers (a room left) falls back to Public.
    fn index_of(&self, answer: &GateAnswer) -> u32 {
        let position = match answer {
            GateAnswer::Public => None,
            GateAnswer::Tier(t) => self.tiers.iter().position(|x| x == t).map(|i| i + 1),
            GateAnswer::Room(r) => self
                .rooms
                .iter()
                .position(|x| &x.room == r)
                .map(|i| i + 1 + self.tiers.len()),
            GateAnswer::Sell => Some(1 + self.tiers.len() + self.rooms.len()),
        };
        position.unwrap_or(0) as u32
    }
}

/// The composer's audience group — the select plus the teaser and the three
/// sale fields — rendered from the snapshot like the text and tags, never
/// page-local: each answer reaches the shared manager as it is picked, so a
/// half-written post's audience rides the posts draft rail across a restart
/// (`ui/feed.md` § Persistence → *Only user-authored input rests*), and
/// `render_posts` paints the restored audience back through [`Self::paint`].
/// tui's `Action::SetGateTier` is the same shape.
#[derive(Clone)]
struct AudienceControls {
    select: gtk::DropDown,
    /// The option model currently shown in `select`: the change detector and
    /// the handlers' index→answer map.
    options: Rc<RefCell<GateOptions>>,
    preview: gtk::Entry,
    price: gtk::Entry,
    asking_price: gtk::Entry,
    subscribers_free: gtk::CheckButton,
}

impl AudienceControls {
    fn answer(&self) -> GateAnswer {
        self.options.borrow().answer(self.select.selected())
    }

    fn sell_fields(&self) -> SellComposeState {
        SellComposeState {
            price: self.price.text().to_string(),
            // Free text, parsed as a whole-sats u64 by
            // `FeedManager::prepare_sell_post` — never derived from `price`
            // above (`monetization.md` § The asking price).
            asking_price: self.asking_price.text().to_string(),
            subscribers_get_it_free: self.subscribers_free.is_active(),
        }
    }

    /// Stage the picked answer (and its teaser) through the shared setters,
    /// which clear each other — so "gated to a tier AND selling" is never
    /// representable.
    fn forward(&self, manager: &LinuxFeedManager) {
        let preview = self.preview.text().to_string();
        match self.answer() {
            GateAnswer::Sell => manager.update_compose_sell(Some(self.sell_fields()), preview),
            GateAnswer::Room(room) => manager.update_compose_room(Some(room), preview),
            GateAnswer::Tier(tier) => manager.update_compose_gate(Some(tier), preview),
            GateAnswer::Public => manager.update_compose_gate(None, preview),
        }
    }

    /// Show `compose`'s audience. Diff-guarded per widget; the caller holds the
    /// composer's `refreshing` flag so no handler forwards this write back.
    fn paint(&self, compose: &fauna_feed::FeedComposeState) {
        let answer = if compose.sell.is_some() {
            GateAnswer::Sell
        } else if let Some(room) = &compose.gate_room {
            GateAnswer::Room(room.clone())
        } else if let Some(tier) = &compose.gate_tier {
            GateAnswer::Tier(tier.clone())
        } else {
            GateAnswer::Public
        };
        // A tier or room the option list does not offer yet (the reload that
        // fills `own_tiers` still in flight) shows as Public until it does;
        // the manager keeps the answer meanwhile, and the next paint lands it.
        let idx = self.options.borrow().index_of(&answer);
        if self.select.selected() != idx {
            self.select.set_selected(idx);
        }
        if self.preview.text().as_str() != compose.gate_preview.as_str() {
            self.preview.set_text(&compose.gate_preview);
        }
        let sell = compose.sell.clone().unwrap_or_default();
        if self.price.text().as_str() != sell.price.as_str() {
            self.price.set_text(&sell.price);
        }
        if self.asking_price.text().as_str() != sell.asking_price.as_str() {
            self.asking_price.set_text(&sell.asking_price);
        }
        if self.subscribers_free.is_active() != sell.subscribers_get_it_free {
            self.subscribers_free
                .set_active(sell.subscribers_get_it_free);
        }
    }
}

thread_local! {
    /// Session-local set of post ids whose muted-collapse the user revealed
    /// (mirrors `conversations::{is_muted_revealed, reveal_muted}`). Render
    /// state, deliberately NOT manager state — the settled collapse-signal
    /// design (topic-factors.md § Implementation status, 2026-07-12):
    /// `FeedManager::is_muted(post_id)` answers whether a post matches, the
    /// shell owns the transient reveal.
    static REVEALED_MUTED_POSTS: RefCell<std::collections::HashSet<String>> =
        RefCell::new(std::collections::HashSet::new());

    /// Session-local set of post ids whose content-`collapse` the viewer revealed
    /// (the `Collapse` verdict's reveal affordance; distinct from the muted set).
    static REVEALED_CONTENT_POSTS: RefCell<std::collections::HashSet<String>> =
        RefCell::new(std::collections::HashSet::new());
}

/// The composed render verdict for a post — the guardian floor, the viewer's
/// own thresholds and the region content policy (family-safety.md § Content
/// policy; region-blocking.md § Where it composes), with the region scorers'
/// factors joined to this post's labels. Both social surfaces (feed here,
/// conversations in `message_bubble.rs`) call the one shared
/// [`crate::region::verdict_for`] so they never drift.
fn composed_verdict(post: &PostSummary) -> fauna_core::obligation::ComposedVerdict {
    crate::region::verdict_for(&post.post_id, &post.labels, || {
        crate::region::post_input(post)
    })
}

/// The region placeholder a post paints in place of its body, when a region
/// policy withholds it — an unrevealed `collapse`, or a `block` (which has no
/// reveal). The ONE check both the list card and the post detail make
/// (`region-blocking.md` § Where it composes: no render surface reaches the
/// composed call for one source while bypassing it for another).
fn region_withheld(post: &PostSummary) -> Option<fauna_client_region::RegionPlaceholder> {
    let withheld = crate::region::region_verdict(&composed_verdict(post))?;
    let revealed = REVEALED_CONTENT_POSTS.with(|s| s.borrow().contains(&post.post_id));
    (withheld.verb == fauna_client_region::RegionVerb::Block || !revealed).then_some(withheld)
}

/// Persistent widget handles for the post-list pane — the bits `refresh`
/// re-targets on every snapshot change (the dynamic *rows* are rebuilt; these
/// containers + the compose surface are built once). No `posts_data`: the
/// shared `FeedManager` snapshot is the only post-list state (priority #1;
/// `feed.md` Architectural rules — no client-side post-list state).
pub struct PostListHandles {
    pub post_list_box: gtk::ListBox,
    pub content_stack: gtk::Stack,
    /// Composer-scoped error (`compose-error`) ← `snapshot.compose.error`.
    pub compose_error_label: gtk::Label,
    /// The compose text field (`compose-text-field`) — `render_posts` pushes
    /// `snapshot.compose.text` in on divergence (draft restore; feed.md §
    /// Persistence), guarded by `compose_refreshing` against the live-sync
    /// handler in `build_compose_post_wired` re-forwarding the programmatic
    /// set back into the manager.
    compose_text_view: gtk::TextView,
    /// The compose tags field (`compose-tags-field`) — same push-on-divergence
    /// treatment as `compose_text_view`, from `snapshot.compose.tags`.
    compose_tags_entry: gtk::Entry,
    /// Shared re-entrancy guard between `render_posts`' programmatic pushes
    /// above and `build_compose_post_wired`'s live-sync handlers.
    compose_refreshing: Rc<Cell<bool>>,
    /// The compose file-ready chip (`compose-file-ready`) ← the manager's own
    /// `snapshot.compose.attached_file`, name + size — a restored draft's
    /// handle included, not only a fresh local pick (`ui/feed.md` §
    /// Persistence → *Attachments by content address*).
    compose_file_ready: gtk::Label,
    /// The compose file's remove control (`compose-file-remove`) — paired
    /// visibility with `compose_file_ready`, both driven by `render_posts`.
    compose_file_remove: gtk::Button,
    /// Page-level error (`error-message`) ← `snapshot.error`.
    pub error_message_label: gtk::Label,
    /// `feed-empty-state` / `feed-no-results` — the post list's placeholder,
    /// at most one shown, ← `snapshot.empty_state()` (`feed.md` § Errors &
    /// edge cases).
    empty_state: adw::StatusPage,
    no_results: adw::StatusPage,
    /// Previous render's `FeedStatus`, so `render_posts` can tell a
    /// `Loading → non-Loading` transition (a *fresh page*) from an
    /// embed-resolution re-emit (`resolve_media` / `resolve_quoted_post`, which
    /// `notify()` while a detail may be open — render-model.md § D6).
    prev_status: Cell<FeedStatus>,
    /// The selection the last fresh page was loaded for, or `None` before the
    /// first one lands. An open detail is dropped only when the selection moves
    /// off it — never on a re-query of the same selection
    /// ([`detail_outlives_render`]).
    settled_selection: RefCell<Option<FeedSelection>>,
    /// The `post_id` of the currently-open post detail, or `None`. Lets
    /// `render_posts` repaint an open detail reactively when a late embed
    /// resolution re-emits (so the folded `QuotedPost` / `Image` blocks appear).
    open_detail_id: Rc<RefCell<Option<String>>>,
    /// The `(has_quoted_post, has_image, has_blocked_remote_images)` embed
    /// signature of the detail as last painted, so a re-emit rebuilds the open
    /// detail **only** when its embeds actually changed — avoiding a gratuitous
    /// rebuild (and scroll reset) on every unrelated notify. The third element
    /// makes a remote-image reveal (D3) repaint the open detail: revealing flips
    /// the document's `revealed` flag, dropping `has_blocked_remote_images` from
    /// `true` to `false`, so the signature changes and the detail re-walks with
    /// the now-fetched picture. The fourth element (`gated_unlocked`) makes a
    /// gated post's async unlock repaint the open detail with the decrypted
    /// full body (`ui/feed.md` § Encryption at rest). The fifth element makes
    /// the tip surface's async resolve (`monetization.md` § Tips) repaint an
    /// already-open detail once `resolve_post_tips` fills it in — the same
    /// class of gap the fourth element closed for the unlock flow.
    open_detail_sig: OpenDetailSig,
    /// The composer's audience group (`compose-gate-tier-select` and its
    /// teaser + sale fields) — its option list is re-driven from
    /// `snapshot.own_tiers` + `snapshot.own_rooms`, and its answer painted from
    /// `snapshot.compose`, by `render_posts`.
    audience: AudienceControls,
}

/// Build the post list pane for the selected feed: compose bar, search bar, the
/// post list, and a "Load more" button. Snapshot-driven — `refresh` (below)
/// rebuilds the rows from `FeedManager::snapshot()`; the gestures drive the
/// manager's async methods.
pub fn build_post_list_wired(
    manager: &Arc<LinuxFeedManager>,
    client: &Rc<FaunaClient>,
    rt: &Handle,
) -> (gtk::Box, PostListHandles) {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);

    // ── Stack to switch between list view and detail view ────────────────
    let content_stack = gtk::Stack::new();
    content_stack.set_transition_type(gtk::StackTransitionType::SlideLeftRight);
    content_stack.set_vexpand(true);

    // ── List page ────────────────────────────────────────────────────────
    let list_page = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let feed_name_label = gtk::Label::new(Some(common::POSTS));
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&feed_name_label));
    list_page.append(&header);

    // Page-level error (`error-message`) — hidden until `snapshot.error` is set.
    let error_message_label = gtk::Label::new(None);
    error_message_label.set_visible(false);
    error_message_label.set_wrap(true);
    error_message_label.add_css_class("error");
    crate::testid::set_test_id(&error_message_label, ids::ERROR_MESSAGE);
    list_page.append(&error_message_label);

    // Compose post area at top.
    let (
        compose_box,
        compose_error_label,
        audience,
        compose_text_view,
        compose_tags_entry,
        compose_refreshing,
        compose_file_ready,
        compose_file_remove,
    ) = build_compose_post_wired(manager, client);
    list_page.append(&compose_box);

    // Feed search bar — a *re-query* of the selected feed (never a client-side
    // filter; `feed.md` § Where logic lives → Search filter).
    let search_bar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    search_bar.set_margin_start(8);
    search_bar.set_margin_end(8);
    search_bar.set_margin_top(4);
    search_bar.set_margin_bottom(4);

    let search_entry = gtk::SearchEntry::new();
    search_entry.set_placeholder_text(Some(feed::post::SEARCH_PLACEHOLDER));
    search_entry.set_hexpand(true);
    crate::testid::set_test_id(&search_entry, ids::FEED_SEARCH_FIELD);
    search_bar.append(&search_entry);

    {
        let m = Arc::clone(manager);
        let rt = rt.clone();
        search_entry.connect_search_changed(move |entry| {
            let query = entry.text().to_string();
            let m = Arc::clone(&m);
            let term = if query.trim().is_empty() {
                None
            } else {
                Some(query)
            };
            rt.spawn(async move { m.set_search_query(term).await });
        });
    }

    let clear_btn = gtk::Button::from_icon_name("edit-clear-symbolic");
    clear_btn.add_css_class("flat");
    clear_btn.set_tooltip_text(Some(common::CLEAR_SEARCH));
    crate::testid::set_test_id(&clear_btn, ids::FEED_SEARCH_CLEAR);
    {
        let entry = search_entry.clone();
        let m = Arc::clone(manager);
        let rt = rt.clone();
        clear_btn.connect_clicked(move |_| {
            entry.set_text("");
            let m = Arc::clone(&m);
            rt.spawn(async move { m.clear_search().await });
        });
    }
    search_bar.append(&clear_btn);
    list_page.append(&search_bar);

    // Scrollable post list.
    let post_list_box = gtk::ListBox::new();
    post_list_box.set_selection_mode(gtk::SelectionMode::Single);
    post_list_box.set_activate_on_single_click(true);
    post_list_box.add_css_class("boxed-list");
    crate::testid::set_test_id(&post_list_box, ids::FEED_VIEW);

    // The empty list's placeholder holds the feed's two empty states
    // (`feed.md` § Errors & edge cases): `render_posts` shows at most one,
    // off the shared `FeedSnapshot::empty_state`, and neither while a read is
    // in flight — so an empty ListBox mid-load paints nothing.
    let empty_state = adw::StatusPage::builder()
        .title(feed::list::NO_POSTS)
        .visible(false)
        .build();
    crate::testid::set_test_id(&empty_state, ids::FEED_EMPTY_STATE);
    let no_results = adw::StatusPage::builder()
        .title(feed::list::NO_MATCHING_POSTS)
        .visible(false)
        .build();
    crate::testid::set_test_id(&no_results, ids::FEED_NO_RESULTS);
    let placeholder = gtk::Box::new(gtk::Orientation::Vertical, 0);
    placeholder.append(&empty_state);
    placeholder.append(&no_results);
    post_list_box.set_placeholder(Some(&placeholder));

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&post_list_box)
        .build();
    list_page.append(&scrolled);

    // Engagement-cue capture (engagement-cues.md): sample each card's honest
    // viewport dwell off the retained ScrolledWindow and report raw
    // observations to the shared engine when a card leaves the viewport.
    crate::feed::viewport::wire(&scrolled, &post_list_box, manager, rt, &error_message_label);

    // Fetch-on-session-start: install the sealed cues:v1 rollup into the
    // manager before observations land (puts are suppressed pre-hydrate).
    // Once per auth by construction — this whole view is rebuilt with the
    // manager on re-auth. An unopenable rollup surfaces on the page error
    // element, never a silent fresh start. Kept: hydrate_cues composes a
    // NestClient RPC with a local unseal, not a single RPC (transport.md §
    // Request lifecycle step 3's note).
    {
        let m = Arc::clone(manager);
        let error_label = error_message_label.clone();
        crate::async_helper::spawn_with_snapshot(
            rt,
            move || async move { crate::async_helper::hydrate_with_retry(|| m.hydrate_cues()).await },
            move |result: Result<(), String>| {
                if let Err(msg) = result {
                    crate::settings::render_error_label(&error_label, Some(&msg));
                }
            },
        );
    }

    // Session-start hydrate of the Layer-B signal-sharing opt-in
    // (engagement-cues.md § Layer B), beside the cue hydrate: the producer must
    // respect a persisted opt-in (set on a prior session or another device) from
    // feed-view build, not only after the user opens the Personalization page.
    // Best-effort — the opt-in defaults off, so a read failure safely leaves the
    // producer silent (the Personalization pane is the authoritative surface,
    // and it surfaces its own errors). `hydrate_signal_optin` is a single
    // NestClient RPC — the transport already parks it while the socket comes
    // up (transport.md § Request lifecycle step 3).
    {
        let m = Arc::clone(manager);
        crate::async_helper::spawn_with_snapshot(
            rt,
            move || async move { m.hydrate_signal_optin().await },
            move |_result: Result<bool, String>| {},
        );
    }

    // Load more button → `FeedManager::load_more`.
    let load_more = gtk::Button::with_label(common::LOAD_MORE);
    load_more.set_halign(gtk::Align::Center);
    load_more.set_margin_top(8);
    load_more.set_margin_bottom(8);
    {
        let m = Arc::clone(manager);
        let rt = rt.clone();
        load_more.connect_clicked(move |_| {
            let m = Arc::clone(&m);
            rt.spawn(async move { m.load_more().await });
        });
    }
    list_page.append(&load_more);

    content_stack.add_named(&list_page, Some("list"));
    outer.append(&content_stack);

    // Reactive-detail bookkeeping (see `PostListHandles`): the open detail's
    // post_id + its last-painted embed signature, shared between row activation
    // and the observer-driven `render_posts` repaint.
    let open_detail_id: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let open_detail_sig: OpenDetailSig = Rc::new(Cell::new((false, false, false, false, false)));

    // ── Row activation → detail view. The current snapshot is read at click
    //    time (rows are appended 1:1 with `snapshot.posts` in `refresh`). ──
    {
        let stack = content_stack.clone();
        let m = Arc::clone(manager);
        let c = Rc::clone(client);
        let rt = rt.clone();
        let open_id = Rc::clone(&open_detail_id);
        let open_sig = Rc::clone(&open_detail_sig);
        post_list_box.connect_row_activated(move |_, row| {
            let idx = row.index().max(0) as usize;
            if let Some(post) = m.snapshot().posts.get(idx).cloned() {
                // A REPOST ROW's own detail would be blank (the repost post is
                // empty by construction) — activation opens the ORIGINAL's
                // detail instead (feed.md § Interaction bar → Repost).
                // `open_post_detail_by_id` resolves the target through
                // `FeedManager::resolve_post` — the same deep-link door a
                // search hit uses (`ui/search.md` § Where logic lives →
                // Result navigation) — so an original outside the loaded
                // window still opens with real content instead of degrading
                // to a no-op the way a raw `.posts.iter().find()` scan would.
                let target_id = post.reposted_post_id.clone().unwrap_or(post.post_id);
                open_post_detail_by_id(&stack, &open_id, &open_sig, &m, &c, &rt, target_id);
            }
        });
    }

    // Convention 17's verdict-side walk (`region::block_render_json`): the list's
    // rows are 1:1 with `snapshot.posts`; an open detail shows one post.
    {
        let stack = content_stack.downgrade();
        let m = Arc::clone(manager);
        let open_id = Rc::clone(&open_detail_id);
        crate::region::register_block_counter(&content_stack, move || {
            let Some(stack) = stack.upgrade() else {
                return 0;
            };
            let snapshot = m.snapshot();
            let blocked = |p: &PostSummary| crate::region::is_region_blocked(&composed_verdict(p));
            match stack.visible_child_name().as_deref() {
                Some("list") => snapshot.posts.iter().filter(|p| blocked(p)).count(),
                Some("detail") => open_id
                    .borrow()
                    .as_deref()
                    .and_then(|id| snapshot.find_post(id))
                    .map_or(0, |p| usize::from(blocked(p))),
                _ => 0,
            }
        });
    }

    let handles = PostListHandles {
        post_list_box,
        content_stack,
        compose_error_label,
        compose_text_view,
        compose_tags_entry,
        compose_refreshing,
        compose_file_ready,
        compose_file_remove,
        error_message_label,
        empty_state,
        no_results,
        prev_status: Cell::new(FeedStatus::default()),
        settled_selection: RefCell::new(None),
        open_detail_id,
        open_detail_sig,
        audience,
    };
    (outer, handles)
}

/// Push a compose `AttachedFile` handle's name + size onto a file-ready chip
/// and pair its remove control's visibility with it — `dm-compose-attachment-
/// chip`'s shape one page over. Shared by `render_posts` (the inline bar,
/// reactive off the manager's own observer loop) and the rich compose
/// dialog's one-shot echo (it carries no observer loop of its own).
fn apply_compose_file_chip(ready: &gtk::Label, remove: &gtk::Button, file: Option<&AttachedFile>) {
    match file {
        Some(file) => {
            ready.set_text(&format!(
                "{} ({})",
                file.name,
                crate::i18n::byte_size(file.size)
            ));
            ready.set_visible(true);
            remove.set_visible(true);
        }
        None => {
            ready.set_visible(false);
            remove.set_visible(false);
        }
    }
}

/// Re-render the post-list pane from a fresh `FeedSnapshot`: rebuild the rows,
/// surface the page-level + composer errors, and kick off the lazy per-post
/// media-hash resolution. Called on every observer notification.
pub fn render_posts(
    handles: &PostListHandles,
    snap: &FeedSnapshot,
    manager: &Arc<LinuxFeedManager>,
    client: &Rc<FaunaClient>,
    rt: &Handle,
) {
    // Page-level error (`error-message`).
    match &snap.error {
        Some(lt) => {
            handles
                .error_message_label
                .set_text(&lt.resolve(crate::i18n::strings::lookup));
            handles.error_message_label.set_visible(true);
        }
        None => handles.error_message_label.set_visible(false),
    }

    // Empty state (`feed-empty-state` / `feed-no-results`) ← the shared
    // decision, never `posts.is_empty()` or the search entry's own text.
    let empty = snap.empty_state();
    handles
        .empty_state
        .set_visible(empty == Some(FeedEmptyState::NoPosts));
    handles
        .no_results
        .set_visible(empty == Some(FeedEmptyState::NoMatches));

    // Compose text/tags/audience ← snapshot (draft restore + cross-notify
    // sync; `ui/feed.md` § Persistence). Diff-guarded to avoid a spurious
    // cursor jump on every unrelated notify, and `compose_refreshing`
    // suppresses `build_compose_post_wired`'s live-sync handlers while this
    // push runs so it can't immediately re-forward its own write back into the
    // manager.
    handles.compose_refreshing.set(true);
    {
        let buf = handles.compose_text_view.buffer();
        let current = buf
            .text(&buf.start_iter(), &buf.end_iter(), false)
            .to_string();
        if current != snap.compose.text {
            buf.set_text(&snap.compose.text);
        }
    }
    if handles.compose_tags_entry.text().as_str() != snap.compose.tags.as_str() {
        handles.compose_tags_entry.set_text(&snap.compose.tags);
    }
    // Audience option set (`compose-gate-tier-select`) ← `snapshot.own_tiers`
    // (refreshed by the manager on every reload) and `snapshot.own_rooms`
    // (also re-read on the conversations plane's own tick —
    // `conv_backend::attach_own_rooms_refresh`). Rebuild the model only on an
    // actual change — inside the `refreshing` span, because `set_model` resets
    // the selection and must not forward that reset as a pick. The answer
    // itself is the manager's, so the paint after it restores it: a room
    // joined mid-compose never snaps the select back to Public.
    {
        let audience = &handles.audience;
        let options = GateOptions {
            tiers: snap.own_tiers.iter().map(|t| t.name.clone()).collect(),
            rooms: snap.own_rooms.clone(),
        };
        if *audience.options.borrow() != options {
            let labels = options.labels();
            let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
            // Replace the model BEFORE touching the widget: `set_model` fires
            // the selection handler, which reads it.
            *audience.options.borrow_mut() = options;
            audience
                .select
                .set_model(Some(&gtk::StringList::new(&labels)));
        }
        audience.paint(&snap.compose);
    }
    handles.compose_refreshing.set(false);

    // Composer error (`compose-error`).
    let compose_error = snap
        .compose
        .error
        .as_ref()
        .map(|lt| lt.resolve(crate::i18n::strings::lookup));
    crate::settings::render_error_label(&handles.compose_error_label, compose_error.as_deref());

    // Compose file (`compose-file-ready` / `compose-file-remove`) ← the
    // manager's own `attached_file`, never the local pick alone: a restored
    // draft's handle has no path behind it on this device, and its submit
    // refuses naming exactly this file (`ui/feed.md` § Persistence →
    // *Attachments by content address*).
    apply_compose_file_chip(
        &handles.compose_file_ready,
        &handles.compose_file_remove,
        snap.compose.attached_file.as_ref(),
    );

    // A *fresh page* is a `Loading → non-Loading` transition. Only that switches
    // back to the list and drops a stale detail; an embed-resolution re-emit
    // (`resolve_media` / `resolve_quoted_post`, which now `notify()` — D6) keeps
    // the user where they are and instead repaints an open detail (below).
    let prev_status = handles.prev_status.replace(snap.status);
    let fresh_page = prev_status == FeedStatus::Loading && snap.status != FeedStatus::Loading;

    // Rebuild rows from the snapshot's ordered, deduplicated list.
    // Rows only: the placeholder holding the empty states must survive.
    let list = &handles.post_list_box;
    crate::views::layout::clear_list_box_rows(list);
    for post in &snap.posts {
        list.append(&build_post_card(
            post,
            manager,
            client,
            rt,
            &handles.error_message_label,
        ));
        // Lazily resolve the media blob hash (a no-op once resolved / for a post
        // with no media); the manager folds an `Image` block into the document +
        // notifies, so the next `render_posts` paints it.
        if post.has_media && post.media_hash.is_none() {
            let m = Arc::clone(manager);
            let id = post.post_id.clone();
            rt.spawn(async move { m.resolve_media(id).await });
        }
        // Lazily resolve the quoted/reposted post — **fire-once**: trigger only
        // while the `QuotedPost` block hasn't been folded into the document
        // yet. Either embed field folds through the same call: a QUOTE embeds
        // its target, a REPOST ROW embeds its original (feed.md § Interaction
        // bar → Repost). The manager folds the block + notifies (idempotently),
        // so the next `render_posts` paints the card via the shared document
        // walker; the guard then stops the trigger, so the re-emit settles
        // instead of looping.
        if let Some(qid) = post
            .quoted_post_id
            .as_ref()
            .or(post.reposted_post_id.as_ref())
            && !post.document.has_quoted_post()
        {
            let m = Arc::clone(manager);
            let id = qid.clone();
            rt.spawn(async move {
                m.resolve_quoted_post(id).await;
            });
        }
        // Lazily resolve link previews — **fire-once** per `Resolving` block (D4). The
        // manager calls `fauna.linkpreview.resolve`, folds the `Resolved` state +
        // notifies (idempotently); the next `render_posts` paints the card via the
        // extracted `resolved_link_previews`. A `Resolving` block is the fire-once guard
        // (it disappears from this list once resolved), so the re-emit settles.
        for url in post.document.resolving_link_preview_urls() {
            let m = Arc::clone(manager);
            let url = url.to_string();
            rt.spawn(async move {
                m.resolve_link_preview(url).await;
            });
        }
        // Lazily resolve the buyer's price read (gap (2c)) — fire-once via the
        // `unlock_offer.is_none()` guard, only for a designated `post-unlock-*`
        // tier (`fauna_client_subscriptions::UNLOCK_TIER_PREFIX`).
        if post.unlock_offer.is_none()
            && post
                .gated_tier
                .as_deref()
                .is_some_and(|t| t.starts_with(fauna_client_subscriptions::UNLOCK_TIER_PREFIX))
        {
            let m = Arc::clone(manager);
            let id = post.post_id.clone();
            rt.spawn(async move { m.resolve_post_unlock_offer(id).await });
        }
        // The tip surface (`monetization.md` § Tips). No data trigger exists —
        // nothing in the feed projection says whether a post has tips — so the
        // guard is the resolved field alone, and the resolve is fire-once
        // *because* it writes a view on every outcome (an untipped post
        // resolves to zeroes).
        //
        // `resolve_post_tips` is the one thing `fauna-feed/payments` gates, so
        // this is the compiler-caught half. `PostSummary.tips` itself is an
        // ungated inert record: without this resolve it stays `None` forever in
        // a store-safe build, which is precisely why the RENDER below carries
        // its own gate too (`dynamic-features.md` § Platform-family surface
        // excision — dead is not absent).
        #[cfg(feature = "payments")]
        if post.tips.is_none() {
            let m = Arc::clone(manager);
            let id = post.post_id.clone();
            rt.spawn(async move { m.resolve_post_tips(id).await });
        }
    }

    // Detail pane. An open detail belongs to the selection it was opened under:
    // picking another feed (or Trending) drops it and shows the list, while a
    // re-query of the SAME selection — this page's own map refresh, a reconnect
    // re-hydrate, a compose re-fetch — keeps it. Dropping on every fresh page
    // wiped a search deep link whenever its resolve landed before the map
    // refresh that navigating here had started (`ui/feed.md`'s `post_detail`,
    // which a `search-result-item` activation opens). A kept detail repaints
    // when its embeds resolved (so a late-folded `QuotedPost` / `Image`
    // appears) — only on an actual embed-signature change, to avoid a
    // gratuitous rebuild + scroll reset.
    let selection = feed_selection(snap);
    let keep_detail =
        detail_outlives_render(handles.settled_selection.borrow().as_ref(), &selection);
    if fresh_page {
        *handles.settled_selection.borrow_mut() = Some(selection);
    }
    if !keep_detail {
        *handles.open_detail_id.borrow_mut() = None;
        if let Some(d) = handles.content_stack.child_by_name("detail") {
            handles.content_stack.remove(&d);
        }
        handles.content_stack.set_visible_child_name("list");
    } else {
        let open = handles.open_detail_id.borrow().clone();
        if let Some(pid) = open {
            // `find_post` (not a raw `.posts.iter().find()` scan of only the
            // loaded list) so a deep-linked post parked in
            // `FeedSnapshot::deep_linked_post` reactively repaints here too —
            // the same union `show_post_detail`'s caller reads.
            if let Some(post) = snap.find_post(&pid) {
                let sig = detail_embed_sig(post);
                if handles.open_detail_sig.get() != sig {
                    show_post_detail(
                        &handles.content_stack,
                        &handles.open_detail_id,
                        &handles.open_detail_sig,
                        post,
                        manager,
                        client,
                        rt,
                    );
                }
            }
        }
    }
}

/// Resolve `post_id` via [`fauna_feed::FeedManager::resolve_post`] — idempotent
/// for a post the timeline already holds, since `resolve_post` finds it in
/// `snapshot.posts` and returns immediately with no round trip — then open its
/// detail pane. The single door for every caller that has only a post's id
/// rather than an on-screen `post-card`: the repost-original branch in the row
/// activation handler above, and a `search-result-item` deep link
/// (`ui/search.md` § Where logic lives → Result navigation (deep link)).
///
/// Runs the resolve on `rt` and marshals the render back onto the GTK main
/// thread (`crate::async_helper::spawn_with_snapshot`), since `resolve_post`
/// is async and GTK widgets are not `Send`. A post that resolves to nothing
/// (deleted, quarantined, unreachable) degrades to a no-op — the same posture
/// every pre-existing `post_detail` open has always taken for a stale id.
#[allow(clippy::too_many_arguments)]
fn open_post_detail_by_id(
    stack: &gtk::Stack,
    open_detail_id: &Rc<RefCell<Option<String>>>,
    open_detail_sig: &OpenDetailSig,
    manager: &Arc<LinuxFeedManager>,
    client: &Rc<FaunaClient>,
    rt: &Handle,
    post_id: String,
) {
    // Fast path: already loaded (the ordinary card-click case) — show now,
    // no round trip, matching every pre-existing detail open.
    if let Some(post) = manager.snapshot().find_post(&post_id).cloned() {
        show_post_detail(
            stack,
            open_detail_id,
            open_detail_sig,
            &post,
            manager,
            client,
            rt,
        );
        return;
    }

    // Slow path: a deep link the timeline never loaded. Switch to the
    // "detail" stack page IMMEDIATELY with a loading placeholder — mirroring
    // tui's `Action::OpenPostDetail`, which flips `app.feed.mode` to
    // `PostDetail` synchronously and lets the async `resolve_post` op fill in
    // content afterward. Without this the detail pane stays invisible for the
    // whole round trip: `feed-post-detail-dialog` never appears until the
    // fetch completes, instead of appearing right away and filling in —
    // exactly the gap `test_activating_a_post_search_result_navigates_to_its_post_detail`
    // measures (ui/search.md § Where logic lives → Result navigation).
    show_post_detail_loading(stack);

    let stack = stack.clone();
    let open_id = Rc::clone(open_detail_id);
    let open_sig = Rc::clone(open_detail_sig);
    let manager_bg = Arc::clone(manager);
    let manager_render = Arc::clone(manager);
    let client_render = Rc::clone(client);
    let rt_render = rt.clone();
    let pid = post_id.clone();
    crate::async_helper::spawn_with_snapshot(
        rt,
        move || async move { manager_bg.resolve_post(pid).await },
        move |_resolution: fauna_feed::PostResolution| {
            if let Some(post) = manager_render.snapshot().find_post(&post_id).cloned() {
                show_post_detail(
                    &stack,
                    &open_id,
                    &open_sig,
                    &post,
                    &manager_render,
                    &client_render,
                    &rt_render,
                );
            }
            // `Unavailable`/no snapshot hit: the loading placeholder stays up
            // (its Back button still works) rather than silently reverting to
            // the list — the same "degrade to a no-op, never crash" posture
            // every pre-existing stale-id open takes, just with a visible
            // parked state instead of nothing happening at all.
        },
    );
}

/// The "detail" stack page shown the instant a deep-linked post-detail open
/// starts, before [`fauna_feed::FeedManager::resolve_post`]'s round trip
/// completes. Carries the SAME `feed-post-detail-dialog` test id
/// [`build_post_detail`] does — the destination is reachable immediately,
/// only the body fills in a beat later — and a working Back button, so a
/// resolve that never completes still leaves the user an exit.
fn show_post_detail_loading(stack: &gtk::Stack) {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    crate::testid::set_test_id(&outer, ids::FEED_POST_DETAIL_DIALOG);

    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&gtk::Label::new(Some(common::FEED))));
    let back_btn = gtk::Button::from_icon_name("go-previous-symbolic");
    back_btn.set_tooltip_text(Some(crate::i18n::strings::common::BACK));
    {
        let s = stack.clone();
        back_btn.connect_clicked(move |_| {
            s.set_visible_child_name("list");
            if let Some(d) = s.child_by_name("detail") {
                s.remove(&d);
            }
        });
    }
    header.pack_start(&back_btn);
    outer.append(&header);

    let spinner = gtk::Spinner::new();
    spinner.set_spinning(true);
    spinner.set_margin_top(32);
    spinner.set_halign(gtk::Align::Center);
    outer.append(&spinner);

    if let Some(old) = stack.child_by_name("detail") {
        stack.remove(&old);
    }
    stack.add_named(&outer, Some("detail"));
    stack.set_visible_child_name("detail");
}

/// The selection a feed page is loaded for: `selected_feed` plus the Trending
/// flag, which is not a `selected_feed` id (`trending.md` § The Trending feed).
type FeedSelection = (Option<String>, bool);

fn feed_selection(snap: &FeedSnapshot) -> FeedSelection {
    (snap.selected_feed.clone(), snap.trending_selected)
}

/// Whether an open post detail survives this render: it does unless the
/// selection has moved off the one the last fresh page was loaded for. With no
/// page settled yet there is nothing to have moved off, so a detail opened
/// before the first page lands — a deep link into a cold feed pane — is kept.
fn detail_outlives_render(settled: Option<&FeedSelection>, current: &FeedSelection) -> bool {
    settled.is_none_or(|s| s == current)
}

/// Build the `search-result-item` deep-link door onto this pane's post
/// detail: a callback that takes a post id and opens it, sharing the exact
/// same resolve + open-detail bookkeeping [`open_post_detail_by_id`] uses —
/// so a search-originated open can never show a DIFFERENT "open" post than
/// what row activation / the reactive repaint in [`render_posts`] are
/// tracking (`ui/search.md` § Where logic lives → Result navigation (deep
/// link)).
pub fn open_post_detail_fn(
    handles: &PostListHandles,
    manager: &Arc<LinuxFeedManager>,
    client: &Rc<FaunaClient>,
    rt: &Handle,
) -> Rc<dyn Fn(String)> {
    let stack = handles.content_stack.clone();
    let open_id = Rc::clone(&handles.open_detail_id);
    let open_sig = Rc::clone(&handles.open_detail_sig);
    let manager = Arc::clone(manager);
    let client = Rc::clone(client);
    let rt = rt.clone();
    Rc::new(move |post_id: String| {
        open_post_detail_by_id(&stack, &open_id, &open_sig, &manager, &client, &rt, post_id);
    })
}

/// Show (or repaint) the post-detail pane for `post`: record it as the open
/// detail + its embed signature, (re)build the detail widget from the current
/// `post.document`, and make it the visible stack child. Shared by row
/// activation and the observer-driven reactive repaint in [`render_posts`]; the
/// `Back` button clears the open-detail bookkeeping and returns to the list.
#[allow(clippy::too_many_arguments)]
fn show_post_detail(
    stack: &gtk::Stack,
    open_detail_id: &Rc<RefCell<Option<String>>>,
    open_detail_sig: &OpenDetailSig,
    post: &PostSummary,
    manager: &Arc<LinuxFeedManager>,
    client: &Rc<FaunaClient>,
    rt: &Handle,
) {
    *open_detail_id.borrow_mut() = Some(post.post_id.clone());
    open_detail_sig.set(detail_embed_sig(post));

    // Gated post not yet unlocked: kick the async unlock (resolve → bulk
    // fetch → decrypt). On success the manager notifies, `gated_unlocked`
    // flips the embed signature, and `render_posts` repaints this detail with
    // the full body — until then the detail shows the plaintext teaser.
    if post.gated_tier.is_some() && !post.gated_unlocked {
        client.unlock_gated_post(Arc::clone(manager), post.post_id.clone());
    }

    if let Some(old) = stack.child_by_name("detail") {
        stack.remove(&old);
    }
    let on_back = {
        let s = stack.clone();
        let open = Rc::clone(open_detail_id);
        move || {
            *open.borrow_mut() = None;
            s.set_visible_child_name("list");
            if let Some(d) = s.child_by_name("detail") {
                s.remove(&d);
            }
        }
    };
    // Post detail composes the region source like the list card does
    // (`region-blocking.md` § Where it composes) — a region-withheld post
    // opened from anywhere shows the placeholder, never the body the card
    // withheld. The reveal of a `collapse` re-opens the detail in full.
    let detail = if let Some(withheld) = region_withheld(post) {
        let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        crate::testid::set_test_id(&outer, ids::FEED_POST_DETAIL_DIALOG);
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&gtk::Label::new(Some(common::FEED))));
        let back_btn = gtk::Button::from_icon_name("go-previous-symbolic");
        back_btn.set_tooltip_text(Some(crate::i18n::strings::common::BACK));
        back_btn.connect_clicked(move |_| on_back());
        header.pack_start(&back_btn);
        outer.append(&header);
        let (s, open, sig, p, m, c, r) = (
            stack.clone(),
            Rc::clone(open_detail_id),
            open_detail_sig.clone(),
            post.clone(),
            Arc::clone(manager),
            Rc::clone(client),
            rt.clone(),
        );
        outer.append(&crate::region::placeholder_box(&withheld, move || {
            REVEALED_CONTENT_POSTS.with(|set| set.borrow_mut().insert(p.post_id.clone()));
            show_post_detail(&s, &open, &sig, &p, &m, &c, &r);
        }));
        outer.upcast::<gtk::Widget>()
    } else {
        build_post_detail(post, manager, client, rt, on_back).upcast::<gtk::Widget>()
    };
    stack.add_named(&detail, Some("detail"));
    stack.set_visible_child_name("detail");
}

/// The `(has_quoted_post, has_image, has_blocked_remote_images, gated_unlocked,
/// has_tips)` embed signature of an open post detail — the reactive-repaint
/// trigger in [`render_posts`]. A change in any element (a late-folded quote /
/// media, a remote-image reveal flipping `has_blocked_remote_images` — D3, a
/// gated post's async unlock swapping the decrypted full body in, or the tip
/// surface's async resolve filling in `post.tips`) re-walks the open detail so
/// the new content appears.
///
/// Deliberately NOT `payments`-gated, though its last member reads `post.tips`.
/// This is a change-detection signature, not a surface: it emits no element id
/// and no kind string, so neither criterion 1 nor 2 of `dynamic-features.md`
/// § What "completely compiled away" means touches it. `PostSummary.tips` is an
/// ungated inert record that stays `None` forever in a store-safe build, so the
/// member is already constant-`false` there — and keeping the tuple's arity the
/// same in both flavors keeps one shape for the cold reader instead of two.
fn detail_embed_sig(post: &PostSummary) -> (bool, bool, bool, bool, bool) {
    (
        post.document.has_quoted_post(),
        post.document.first_image_hash().is_some(),
        post.document.has_blocked_remote_images(),
        post.gated_unlocked,
        post.tips.is_some(),
    )
}

/// Compose area at top of post list: text + tags + attach + post, submitting
/// through `FeedManager::submit_post`. Returns the box and the `compose-error`
/// label (which `render_posts` drives from `snapshot.compose.error`).
fn build_compose_post_wired(
    manager: &Arc<LinuxFeedManager>,
    client: &Rc<FaunaClient>,
) -> ComposePostWidgets {
    // Set by `render_posts` while it pushes the snapshot into these widgets,
    // so no live-sync handler below forwards that push back into the manager.
    let refreshing: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    let text_view = gtk::TextView::new();
    text_view.set_wrap_mode(gtk::WrapMode::Word);
    text_view.set_top_margin(8);
    text_view.set_bottom_margin(8);
    text_view.set_left_margin(8);
    text_view.set_right_margin(8);
    text_view.set_vexpand(false);
    crate::testid::set_test_id(&text_view, ids::COMPOSE_TEXT_FIELD);

    // Bound the body scroller (same fix as the conversations compose bar): a
    // GtkScrolledWindow drops its child's `size_request` in the scroll
    // direction, so clamp to 60–180px and grow with content, then scroll.
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .min_content_height(60)
        .max_content_height(180)
        .propagate_natural_height(true)
        .child(&text_view)
        .hexpand(true)
        .build();

    // File-ready chip + its remove control — both hidden until the manager's
    // own `attached_file` names one, driven together by `render_posts` off
    // `snapshot.compose.attached_file` (a restored draft's handle included,
    // not only a fresh local pick — `ui/feed.md` § Persistence → *Attachments
    // by content address*).
    let file_ready_marker = gtk::Label::new(None);
    file_ready_marker.set_visible(false);
    file_ready_marker.add_css_class("caption");
    crate::testid::set_test_id(&file_ready_marker, ids::COMPOSE_FILE_READY);

    let file_remove_btn = gtk::Button::from_icon_name("window-close-symbolic");
    file_remove_btn.add_css_class("flat");
    file_remove_btn.set_visible(false);
    crate::testid::set_test_id(&file_remove_btn, ids::COMPOSE_FILE_REMOVE);
    {
        let m = Arc::clone(manager);
        let c = Rc::clone(client);
        file_remove_btn.connect_clicked(move |_| {
            c.clear_staged_attachment();
            let snap = m.snapshot();
            m.update_compose(snap.compose.text, snap.compose.tags, None);
        });
    }

    // Attach button — stage a file (the blob uploads at post time; the upload
    // is client glue, the post build is the manager's — `feed.md` § Where logic
    // lives → Image upload). `stage_attachment` itself writes the hash-less
    // handle through to the manager, so `render_posts` picks up the chip —
    // nothing imperative to do here.
    let attach_btn = gtk::Button::from_icon_name("mail-attachment-symbolic");
    attach_btn.set_tooltip_text(Some(feed::post::ATTACH_IMAGE));
    attach_btn.add_css_class("flat");
    attach_btn.set_valign(gtk::Align::End);
    attach_btn.set_margin_start(4);
    crate::testid::set_test_id(&attach_btn, ids::COMPOSE_FILE);

    {
        let c = Rc::clone(client);
        attach_btn.connect_clicked(move |btn| {
            let dialog = gtk::FileDialog::builder()
                .title(feed::post::ATTACH_IMAGE)
                .build();

            let filter = gtk::FileFilter::new();
            filter.set_name(Some("Images (*.png, *.jpg, *.webp, *.gif)"));
            filter.add_mime_type("image/png");
            filter.add_mime_type("image/jpeg");
            filter.add_mime_type("image/webp");
            filter.add_mime_type("image/gif");
            let filters = gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);
            dialog.set_filters(Some(&filters));
            dialog.set_default_filter(Some(&filter));

            let win = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
            let client_for_upload = Rc::clone(&c);
            dialog.open(win.as_ref(), gio::Cancellable::NONE, move |result| {
                if let Ok(file) = result
                    && let Some(path) = file.path()
                {
                    let path_str = path.to_string_lossy().to_string();
                    client_for_upload.stage_attachment(&path_str);
                }
            });
        });
    }

    // Ctrl+V image paste: stage the pasted image for the next post.
    {
        let c = Rc::clone(client);
        crate::clipboard::setup_image_paste(&text_view, move |path_str| {
            c.stage_attachment(&path_str);
        });
    }

    // Tags entry.
    let tags_entry = gtk::Entry::new();
    tags_entry.set_placeholder_text(Some(feed::post::TAGS_PLACEHOLDER));
    crate::testid::set_test_id(&tags_entry, ids::COMPOSE_TAGS_FIELD);

    // Gate-to-tier controls (`ui/feed.md` § Encryption at rest;
    // monetization.md § Pillars 2+3 — IDs user-approved 2026-07-12; sell option
    // 2026-07-29; room options 2026-09-10): an audience select (Public, the
    // author's own tiers, their rooms, then "Sell this post…" — options driven
    // by `snapshot.own_tiers` + `own_rooms` in `render_posts`, with Sell always
    // the last entry; see [`GateOptions`]) and the public-teaser entry shown
    // while any restricted answer is selected.
    let gate_options: Rc<RefCell<GateOptions>> = Rc::new(RefCell::new(GateOptions::default()));
    let gate_model = gtk::StringList::new(&[feed::post::GATE_PUBLIC, feed::post::GATE_SELL]);
    let gate_select = gtk::DropDown::new(Some(gate_model), gtk::Expression::NONE);
    gate_select.set_tooltip_text(Some(feed::post::GATE_AUDIENCE));
    crate::testid::set_test_id(&gate_select, ids::COMPOSE_GATE_TIER_SELECT);

    let gate_preview_entry = gtk::Entry::new();
    gate_preview_entry.set_placeholder_text(Some(feed::post::GATE_PREVIEW_PLACEHOLDER));
    gate_preview_entry.set_hexpand(true);
    gate_preview_entry.set_visible(false);
    crate::testid::set_test_id(&gate_preview_entry, ids::COMPOSE_GATE_PREVIEW_FIELD);

    // "Sell this post…" controls (`monetization.md` § Per-post pay-to-unlock;
    // IDs user-approved 2026-07-29) — visible only while the select's LAST
    // option (Sell, always one past the tier list) is chosen.
    let sell_price_entry = gtk::Entry::new();
    sell_price_entry.set_placeholder_text(Some(feed::post::SELL_PRICE_PLACEHOLDER));
    sell_price_entry.set_hexpand(true);
    sell_price_entry.set_visible(false);
    crate::testid::set_test_id(&sell_price_entry, ids::COMPOSE_SELL_PRICE);

    // The machine-comparable price (`monetization.md` § The asking price) —
    // independent of `sell_price_entry` above (the free-text hint); no
    // parsing ever infers one from the other. Empty means no machine price:
    // the minted tier stays a tip target forever.
    let sell_asking_price_entry = gtk::Entry::new();
    sell_asking_price_entry.set_placeholder_text(Some(feed::post::SELL_ASKING_PRICE_PLACEHOLDER));
    sell_asking_price_entry.set_hexpand(true);
    sell_asking_price_entry.set_visible(false);
    crate::testid::set_test_id(&sell_asking_price_entry, ids::COMPOSE_SELL_ASKING_PRICE);

    // Defaults CHECKED (user-ratified 2026-07-29): an existing paying
    // subscriber is not charged twice for a post their subscription would
    // reasonably cover, so pay-per-view is the deliberate opt-in.
    let sell_subscribers_free_check =
        gtk::CheckButton::with_label(feed::post::SELL_SUBSCRIBERS_FREE);
    sell_subscribers_free_check.set_active(true);
    sell_subscribers_free_check.set_visible(false);
    crate::testid::set_test_id(
        &sell_subscribers_free_check,
        ids::COMPOSE_SELL_SUBSCRIBERS_FREE,
    );

    let audience = AudienceControls {
        select: gate_select,
        options: gate_options,
        preview: gate_preview_entry,
        price: sell_price_entry,
        asking_price: sell_asking_price_entry,
        subscribers_free: sell_subscribers_free_check,
    };

    // Visibility follows the shown answer, whoever set it (a pick, or
    // `render_posts` painting a restored draft); the forward to the manager
    // only follows a pick.
    {
        let a = audience.clone();
        let m = Arc::clone(manager);
        let refreshing = Rc::clone(&refreshing);
        audience.select.connect_selected_notify(move |_| {
            let answer = a.answer();
            let is_sell = answer == GateAnswer::Sell;
            a.preview.set_visible(answer != GateAnswer::Public);
            a.price.set_visible(is_sell);
            a.asking_price.set_visible(is_sell);
            a.subscribers_free.set_visible(is_sell);
            if !refreshing.get() {
                a.forward(&m);
            }
        });
    }
    // The teaser is shared by every restricted answer and is no part of any
    // key, so it goes through the setter that touches no answer.
    {
        let m = Arc::clone(manager);
        let refreshing = Rc::clone(&refreshing);
        audience.preview.connect_changed(move |entry| {
            if !refreshing.get() {
                m.update_compose_preview(entry.text().to_string());
            }
        });
    }
    // The three sale fields re-stage the sale while it is the answer.
    for entry in [&audience.price, &audience.asking_price] {
        let a = audience.clone();
        let m = Arc::clone(manager);
        let refreshing = Rc::clone(&refreshing);
        entry.connect_changed(move |_| {
            if !refreshing.get() && a.answer() == GateAnswer::Sell {
                a.forward(&m);
            }
        });
    }
    {
        let a = audience.clone();
        let m = Arc::clone(manager);
        let refreshing = Rc::clone(&refreshing);
        audience.subscribers_free.connect_toggled(move |_| {
            if !refreshing.get() && a.answer() == GateAnswer::Sell {
                a.forward(&m);
            }
        });
    }

    let post_btn = gtk::Button::with_label(composer::NEW_POST);
    post_btn.add_css_class("suggested-action");
    post_btn.set_valign(gtk::Align::End);
    post_btn.set_margin_start(8);
    crate::testid::set_test_id(&post_btn, ids::POST_SUBMIT_BUTTON);
    crate::offline_gate::declare_wire_kind(&post_btn, "fauna.posts.create");

    {
        let m = Arc::clone(manager);
        let c = Rc::clone(client);
        let tv = text_view.clone();
        let te = tags_entry.clone();
        post_btn.connect_clicked(move |_| {
            let buffer = tv.buffer();
            let start = buffer.start_iter();
            let end = buffer.end_iter();
            let text = buffer.text(&start, &end, false).to_string();
            let tags = te.text().to_string();
            if text.trim().is_empty() {
                return;
            }
            // The audience is the manager's, never re-read off the select:
            // every pick already reached it through `AudienceControls::forward`,
            // and a restored tier or room the select cannot show yet paints as
            // Public — re-reading the control published such a draft public
            // (`FaunaClient::submit_post`). The compose dialog posts the same.
            //
            // The manager validates, builds + signs (tags → facets, not body
            // text), uploads the staged blob, creates the post, then refreshes
            // the list + clears compose — `render_posts` then drives the text,
            // tags, file chip and audience from the snapshot (never an
            // unconditional clear here): the submit runs on a tokio thread, so
            // clearing the text buffer synchronously would forward an empty
            // edit through `connect_changed` below and race the attachment
            // handle a refused submit still needs to read and re-show
            // (`client.rs::submit_post`). A failure surfaces in
            // `compose-error` via the snapshot. Whether the audience clears
            // with the sent post is the shared `FeedComposeState::clear_sent`'s
            // verdict — only when the composer is otherwise untouched
            // (`ui/feed.md` § User actions, `post-submit-button`) — and the
            // paint shows it.
            c.submit_post(Arc::clone(&m), text, tags);
        });
    }

    // Compose dialog button — opens a rich compose dialog.
    let compose_dialog_btn = gtk::Button::from_icon_name("document-edit-symbolic");
    compose_dialog_btn.add_css_class("flat");
    compose_dialog_btn.set_tooltip_text(Some(feed::post::OPEN_RICH_COMPOSE));
    compose_dialog_btn.set_valign(gtk::Align::End);
    compose_dialog_btn.set_margin_start(4);
    crate::testid::set_test_id(&compose_dialog_btn, ids::COMPOSE_DIALOG_BUTTON);

    {
        let m = Arc::clone(manager);
        let c = Rc::clone(client);
        compose_dialog_btn.connect_clicked(move |btn| {
            let dialog = build_feed_compose_dialog(&m, &c);
            if let Some(root) = btn.root()
                && let Some(win) = root.downcast_ref::<gtk::Window>()
            {
                dialog.set_transient_for(Some(win));
            }
            dialog.present();
        });
    }

    let btn_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    btn_box.set_valign(gtk::Align::End);
    btn_box.append(&attach_btn);
    btn_box.append(&compose_dialog_btn);
    btn_box.append(&post_btn);

    let compose_area = gtk::Box::new(gtk::Orientation::Vertical, 4);
    compose_area.set_margin_top(8);
    compose_area.set_margin_bottom(8);
    compose_area.set_margin_start(8);
    compose_area.set_margin_end(8);

    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    hbox.append(&scroll);
    hbox.append(&btn_box);
    compose_area.append(&hbox);

    // Gate row: audience select + (conditional) public-teaser entry.
    let gate_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    gate_row.append(&audience.select);
    gate_row.append(&audience.preview);
    compose_area.append(&gate_row);

    // Sell row: price + asking price + subscribers-free toggle, visible only
    // in sell mode.
    let sell_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    sell_row.append(&audience.price);
    sell_row.append(&audience.asking_price);
    sell_row.append(&audience.subscribers_free);
    compose_area.append(&sell_row);

    compose_area.append(&tags_entry);
    compose_area.append(&file_ready_marker);
    compose_area.append(&file_remove_btn);

    // Error indicator for the compose area (`compose-error`, hidden by default;
    // driven by `render_posts` from `snapshot.compose.error`).
    let compose_error_label = gtk::Label::new(None);
    compose_error_label.set_visible(false);
    compose_error_label.set_wrap(true);
    compose_error_label.add_css_class("error");
    crate::testid::set_test_id(&compose_error_label, ids::COMPOSE_ERROR);
    compose_area.append(&compose_error_label);

    // Live-forward text/tags into `FeedManager::update_compose` on every edit
    // (`ui/feed.md` § Persistence) — purely a shadow sync feeding the drafts
    // autosave (`feed/drafts.rs`); `post_btn`'s submit handler above still
    // reads the widgets directly and is unaffected. `refreshing` guards
    // against a feedback loop when `render_posts` pushes a restored draft
    // back into these widgets below (the exact pattern
    // `views::conversations::compose_bar::ComposeBar::render` uses).
    // `attached_file` is forwarded from the manager's own current value
    // (never `None`) — a keystroke must not silently drop a staged or
    // restored attachment before submit reads it (`ui/feed.md` § Persistence
    // → *Attachments by content address*; this is exactly the drop the
    // now-removed unconditional post-submit clear used to race).
    {
        let m = Arc::clone(manager);
        let refreshing = Rc::clone(&refreshing);
        let te = tags_entry.clone();
        text_view.buffer().connect_changed(move |buf| {
            if refreshing.get() {
                return;
            }
            let text = buf
                .text(&buf.start_iter(), &buf.end_iter(), false)
                .to_string();
            let attached = m.snapshot().compose.attached_file;
            m.update_compose(text, te.text().to_string(), attached);
        });
    }
    {
        let m = Arc::clone(manager);
        let refreshing = Rc::clone(&refreshing);
        let tv = text_view.clone();
        tags_entry.connect_changed(move |entry| {
            if refreshing.get() {
                return;
            }
            let buf = tv.buffer();
            let text = buf
                .text(&buf.start_iter(), &buf.end_iter(), false)
                .to_string();
            let attached = m.snapshot().compose.attached_file;
            m.update_compose(text, entry.text().to_string(), attached);
        });
    }

    (
        compose_area,
        compose_error_label,
        audience,
        text_view,
        tags_entry,
        refreshing,
        file_ready_marker,
        file_remove_btn,
    )
}

/// Build a `post-image` element from a resolved media blob hash: a flat button
/// (so AT-SPI enumerates it and a click opens the lightbox) wrapping a
/// `GtkPicture` that lazily loads the blob bytes, plus the `c2pa-badge`
/// provenance mark below it (`ui/media.md` § C2PA provenance). The hash comes
/// from the snapshot's `PostSummary.media_hash` (resolved by
/// `FeedManager::resolve_media` from the decoded post body, or folded in by a
/// gated post's detail-open unlock), not `[media:]` markup. The fetched bytes
/// go through the manager's open before they are decoded, whatever kind of post
/// this is — [`crate::feed::post_media`] says why.
///
/// The decoded texture and the badge verdict come from the signed-in reader's
/// per-hash cache ([`crate::media_loads`]): this card is rebuilt on every
/// snapshot notification, and a card that fetched for itself re-downloaded,
/// re-opened and re-decoded its image — on the GTK thread — every time. Only
/// the first card to ask for a hash loads it; one rebuilt while that load is in
/// flight is painted when it lands, one rebuilt after it paints at once. The
/// feed list card and `feed.post_detail` both build through here, so they
/// share one cache.
pub(crate) fn build_post_image(
    hash: &str,
    manager: &Arc<LinuxFeedManager>,
    client: &Rc<FaunaClient>,
) -> gtk::Box {
    build_post_image_from(hash, manager, client.as_ref())
}

/// Where a `post-image` card's two nest reads come from — `FaunaClient` in the
/// app, a counting fake in the tests that pin how many a repaint issues. The
/// blob GET is the document walk's own seam ([`document::BlobSource`], which
/// the og:image reads through too); this adds the C2PA check.
pub(crate) trait PostMediaSource: document::BlobSource {
    /// The `c2pa-badge` verdict for the blob.
    fn fetch_has_c2pa(&self, hash: &str) -> async_channel::Receiver<Result<bool, ApiError>>;

    /// `GET <path>` on the reader's own nest — a bridged post's
    /// `ProxiedImage` (render-model.md § D6c), fetched exactly as a blob is.
    fn fetch_proxied_bytes(&self, path: &str)
    -> async_channel::Receiver<Result<Vec<u8>, ApiError>>;
}

impl PostMediaSource for FaunaClient {
    fn fetch_has_c2pa(&self, hash: &str) -> async_channel::Receiver<Result<bool, ApiError>> {
        FaunaClient::fetch_has_c2pa(self, hash)
    }

    fn fetch_proxied_bytes(
        &self,
        path: &str,
    ) -> async_channel::Receiver<Result<Vec<u8>, ApiError>> {
        FaunaClient::fetch_nest_bytes(self, path.to_string())
    }
}

fn build_post_image_from(
    hash: &str,
    manager: &Arc<LinuxFeedManager>,
    client: &dyn PostMediaSource,
) -> gtk::Box {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 2);
    outer.set_halign(gtk::Align::Start);

    let (button, picture) = post_image_button();
    outer.append(&button);

    match media_loads::ask_image(manager, hash, &picture) {
        Ask::Ready(texture) => picture.set_paintable(Some(&texture)),
        Ask::Start => {
            let rx = client.fetch_blob_bytes(hash);
            let manager = Arc::clone(manager);
            let item_hash = hash.to_string();
            glib::spawn_future_local(async move {
                // A closed channel (the runtime torn down under the fetch) is a
                // transport failure like any other.
                let fetched = rx
                    .recv()
                    .await
                    .unwrap_or_else(|e| Err(ApiError::Transport(e.to_string())));
                // No is-this-post-gated branch: a public post's bytes come
                // straight back, a gated post's sealed item comes back opened.
                // A refused GET, an item that did not open or bytes that do not
                // decode leave every waiting picture empty — terminally, for
                // this reader's session; a nest that could not serve it right
                // now leaves them empty until the next rebuild asks again.
                let texture =
                    crate::main_loop_meter::dispatch("feed-image-decode", String::new, || {
                        match crate::feed::post_media::open_fetched_post_image(
                            &manager, &item_hash, fetched,
                        ) {
                            Finished::Loaded(bytes) => {
                                gtk::gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes))
                                    .ok()
                                    .into()
                            }
                            Finished::Failed => Finished::Failed,
                            Finished::Transient => Finished::Transient,
                        }
                    });
                media_loads::finish_image(&manager, &item_hash, texture);
            });
        }
        Ask::Waiting | Ask::Failed => {}
    }

    // `c2pa-badge` — hidden until this reader's verdict for the hash resolves
    // `true`, so it never flashes on speculatively; an errored or negative
    // check shows no badge (§ C2PA provenance). The verdict is cached per hash
    // beside the texture, so a rebuilt card reads it rather than asking again.
    // Its value is still the blob's `x-c2pa` header alone (a HEAD via the same
    // shared `head_has_c2pa` tui uses) — the uploader's hint, which `ui/media.md`
    // § C2PA provenance requires the viewer to correct against the bytes
    // (`fauna_media::process::detect_c2pa_in_bytes`, the header kept only as a
    // pre-filter, tui's two-stage `Op::FetchC2pa`).
    // That correction replaces what the `Ask::Start` arm below resolves, not
    // the cache: the entry stays one `bool` per hash.
    let badge = gtk::Label::new(Some(c2pa::BADGE_LABEL));
    badge.add_css_class("dim-label");
    badge.set_halign(gtk::Align::Start);
    badge.set_tooltip_text(Some(
        crate::i18n::strings::conversations::detail::BADGE_C2PA,
    ));
    badge.set_visible(false);
    crate::testid::set_test_id(&badge, ids::C2PA_BADGE);
    outer.append(&badge);

    match media_loads::ask_c2pa(manager, hash, &badge) {
        Ask::Ready(verdict) => badge.set_visible(verdict),
        Ask::Start => {
            let rx = client.fetch_has_c2pa(hash);
            let manager = Arc::clone(manager);
            let item_hash = hash.to_string();
            glib::spawn_future_local(async move {
                let verdict = match rx.recv().await {
                    Ok(Ok(verdict)) => Finished::Loaded(verdict),
                    Ok(Err(e)) if e.is_transient() => Finished::Transient,
                    Ok(Err(_)) => Finished::Failed,
                    // The runtime torn down under the check.
                    Err(_) => Finished::Transient,
                };
                media_loads::finish_c2pa(&manager, &item_hash, verdict);
            });
        }
        Ask::Waiting | Ask::Failed => {}
    }

    connect_lightbox(&button, &picture);

    outer
}

/// The `post-image` element's frame — a flat button (so AT-SPI enumerates it and
/// a click opens the lightbox) wrapping the picture a load paints — shared by a
/// blob image and a bridged post's `ProxiedImage`.
fn post_image_button() -> (gtk::Button, gtk::Picture) {
    let button = gtk::Button::new();
    button.add_css_class("flat");
    button.set_margin_top(4);
    button.set_halign(gtk::Align::Start);
    crate::testid::set_test_id(&button, ids::POST_IMAGE);

    let picture = gtk::Picture::new();
    picture.set_can_shrink(true);
    picture.set_size_request(-1, 240);
    button.set_child(Some(&picture));
    (button, picture)
}

/// Open the `image-lightbox` on a `post-image` click.
fn connect_lightbox(button: &gtk::Button, picture: &gtk::Picture) {
    // Clicking a post image opens the `image-lightbox` — the unified surface on
    // web (`C2paImage`'s onclick), apple (`PostCardView`/`MacPostCardView`) and
    // tui, and what ui.yaml's `image-lightbox` component describes. Until
    // 2026-07-30 this instead wrote the blob to a hard-coded `/work/tmp/…` path
    // and opened nothing, which is why `views/lightbox.rs::show_image_lightbox`
    // had zero callers from the feed and ui.yaml called the split "a real
    // priority-#1 divergence, not just a coverage gap".
    //
    // The already-decoded texture is reused rather than re-fetched: it is the
    // same blob, already on the `gtk::Picture` this button wraps (painted from
    // the cache on a rebuilt card) — and for a gated post it is the OPENED blob,
    // which a second GET would not be.
    let pic_for_click = picture.clone();
    button.connect_clicked(move |b| {
        let Some(paintable) = pic_for_click.paintable() else {
            // Bytes still in flight (or undecodable) — nothing to enlarge yet.
            // The card keeps its empty picture, exactly as before the click.
            return;
        };
        let parent = b.root().and_then(|r| r.downcast::<gtk::Window>().ok());
        crate::views::lightbox::show_image_lightbox_paintable(
            parent.as_ref(),
            &paintable,
            c2pa::IMAGE_VIEWER,
        );
    });
}

/// Build a `post-image` element for a bridged post's own picture — the first
/// `ProxiedImage` of a post with no blob image
/// ([`RenderDocument::proxied_post_image`](fauna_core::render::RenderDocument::proxied_post_image),
/// render-model.md § D6c). Its nest-relative `path` is fetched from the
/// reader's own nest through the same bearer-carrying content `get` a blob
/// takes, and the bytes are painted as they come: the proxy's answer is
/// plaintext, so there is no `open_media_bytes`, and no `c2pa-badge` (they are
/// the proxy's live answer, never a stored blob the check could address). The
/// decoded texture shares the per-reader cache, keyed by the path, which
/// cannot collide with a hex blob hash. The element's automation text is the
/// path — the observable tui's label and apple's placeholder carry.
pub(crate) fn build_post_proxied_image(
    path: &str,
    manager: &Arc<LinuxFeedManager>,
    client: &Rc<FaunaClient>,
) -> gtk::Box {
    build_post_proxied_image_from(path, manager, client.as_ref())
}

fn build_post_proxied_image_from(
    path: &str,
    manager: &Arc<LinuxFeedManager>,
    client: &dyn PostMediaSource,
) -> gtk::Box {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 2);
    outer.set_halign(gtk::Align::Start);

    let (button, picture) = post_image_button();
    crate::testid::set_test_text(&button, path);
    outer.append(&button);

    match media_loads::ask_image(manager, path, &picture) {
        Ask::Ready(texture) => picture.set_paintable(Some(&texture)),
        Ask::Start => {
            let rx = client.fetch_proxied_bytes(path);
            let manager = Arc::clone(manager);
            let item_path = path.to_string();
            glib::spawn_future_local(async move {
                let fetched = rx
                    .recv()
                    .await
                    .unwrap_or_else(|e| Err(ApiError::Transport(e.to_string())));
                let texture =
                    crate::main_loop_meter::dispatch("feed-image-decode", String::new, || {
                        match fetched {
                            Ok(bytes) => {
                                gtk::gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes))
                                    .ok()
                                    .into()
                            }
                            Err(e) if e.is_transient() => Finished::Transient,
                            Err(_) => Finished::Failed,
                        }
                    });
                media_loads::finish_image(&manager, &item_path, texture);
            });
        }
        Ask::Waiting | Ask::Failed => {}
    }

    connect_lightbox(&button, &picture);
    outer
}

/// Build a `video-thumbnail` element for a video attachment's content hash
/// (render-model.md § D6b). No poster frame exists to paint — `MediaItem`'s
/// `thumbnail`/`dimensions` are `None` from every writer, deliberately (a
/// poster field would be dead on arrival) — so this mirrors tui's play-glyph
/// text rather than inventing one; a real decoded frame would need a
/// GStreamer-backed `gtk::Video`, a dependency this app doesn't carry for one
/// element. `first_video_hash()` is the exact twin of `first_image_hash()`
/// this function's sibling `build_post_image` reads.
///
/// A bridged post's `ProxiedVideo` (render-model.md § D6c → *Proxied video*)
/// paints here too, its nest-relative path in the hash's place
/// ([`RenderDocument::proxied_post_video`](fauna_core::render::RenderDocument::proxied_post_video));
/// nothing is fetched for either.
pub(crate) fn build_post_video(hash: &str) -> gtk::Box {
    let outer = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    outer.set_halign(gtk::Align::Start);
    outer.set_margin_top(4);
    crate::testid::set_test_id(&outer, ids::VIDEO_THUMBNAIL);

    let icon = gtk::Image::from_icon_name("media-playback-start-symbolic");
    outer.append(&icon);

    let label = gtk::Label::new(Some(hash));
    label.add_css_class("dim-label");
    outer.append(&label);

    outer
}

/// Build the source-protocol badges (`protocol-badge`) for a post's wire
/// `source` field via the shared [`classify_sources`], painting each source's
/// shared [`SourceGlyph`](fauna_core::source_glyph::SourceGlyph) concept as an
/// emoji in a `gtk::Label` — the SAME `source_glyph_emoji` map the conversations
/// rail uses, so the feed badge and the rail can't drift (D5;
/// `docs/goal/architecture/render-model.md` § Deltas). One `protocol-badge` per
/// classified source; the tooltip carries the canonical `SourceKind::label`.
/// (GTK's symbolic-icon theme has no fox / butterfly glyph, so the emoji family
/// — not icon names — is the only one that can render the canonical concept.)
pub fn build_protocol_badges(source: &str) -> Vec<gtk::Label> {
    // No bridges roster yet (`ui/feed.md` § Implementation status today, `SourceKind::Bridged`).
    classify_sources(source, &[])
        .iter()
        .map(|kind| {
            let label =
                gtk::Label::new(Some(crate::source_glyph::source_glyph_emoji(kind.glyph())));
            label.add_css_class("dim-label");
            label.set_tooltip_text(Some(&kind.label()));
            crate::testid::set_test_id(&label, ids::PROTOCOL_BADGE);
            label
        })
        .collect()
}

/// Build the muted "unverified source" badge (`unverified-source-badge`) for a
/// post whose signed envelope **this client** could not verify
/// (`PostSummary::verification == VerificationStatus::Failed`;
/// `docs/goal/architecture/security.md` § App display of unverified content).
/// Returns `None` for [`Unchecked`](VerificationStatus::Unchecked) (the default —
/// a trusted nest-index projection with no envelope to verify) and
/// [`Verified`](VerificationStatus::Verified), so no badge renders. The post body
/// still renders in full either way — a key-rotation-lag false-negative must not
/// make a legitimate post vanish; the badge is the visible caveat, the DKIM-fail
/// analogue.
pub fn build_unverified_badge(verification: VerificationStatus) -> Option<gtk::Label> {
    if verification != VerificationStatus::Failed {
        return None;
    }
    let label = gtk::Label::new(Some(&format!("\u{26A0} {}", feed::UNVERIFIED_SOURCE)));
    label.add_css_class("dim-label");
    label.add_css_class("warning");
    label.set_tooltip_text(Some(feed::UNVERIFIED_SOURCE_TOOLTIP));
    crate::testid::set_test_id(&label, ids::UNVERIFIED_SOURCE_BADGE);
    Some(label)
}

/// Build the "via connected app" badge (`delegated-origin-badge`) for a post an
/// **external app** authored as the account through the D10 delegated authoring
/// sub-key (`PostSummary::authoring_origin == AuthoringOriginStatus::Delegated`;
/// `docs/goal/behavior/atproto-pds-full.md` § Problem 1 → D10 → *Audit*). This is
/// what makes the grant *audited* rather than merely revocable: the signed bytes
/// **are** the log, read client-side, so a user scrolling their own feed can tell
/// which posts they did not personally write.
///
/// The structural twin of [`build_unverified_badge`], one field over — and the
/// gate is deliberately narrower than "not Direct". [`Unknown`] covers **both**
/// the undecoded nest-index list card *and* the verification-**failed** case, and
/// badging the latter would let a forged wire describe its own origin: an
/// unverified envelope's `signer_auth` cert is precisely the part nothing
/// authenticated. So: badge **iff** [`Delegated`], never on [`Unknown`].
///
/// The badge names the *fact*, never an app — one authoring sub-key is minted per
/// account, so nothing in the signed bytes says which app wrote the post.
///
/// [`Unknown`]: AuthoringOriginStatus::Unknown
/// [`Delegated`]: AuthoringOriginStatus::Delegated
pub fn build_delegated_origin_badge(origin: AuthoringOriginStatus) -> Option<gtk::Label> {
    if origin != AuthoringOriginStatus::Delegated {
        return None;
    }
    let label = gtk::Label::new(Some(&format!("\u{1F517} {}", feed::DELEGATED_ORIGIN)));
    label.add_css_class("dim-label");
    label.add_css_class("caption");
    label.set_tooltip_text(Some(feed::DELEGATED_ORIGIN_TOOLTIP));
    crate::testid::set_test_id(&label, ids::DELEGATED_ORIGIN_BADGE);
    Some(label)
}

/// Icon + bare-number content shared by every engagement indicator: icon on
/// the left, a `caption`-styled count label on the right, hidden at 0
/// (ratified 2026-06-27). Unwrapped so a caller can wrap it in a
/// `gtk::Button` (interactive — [`interaction_button`] below) or use it bare
/// (static — no action path exists, e.g. a Bluesky-bridged thread-ancestor
/// card).
pub(crate) fn engagement_indicator_content(icon_name: &str, count: i64) -> gtk::Box {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.set_pixel_size(14);
    content.append(&icon);

    // The count rides a child label so it can be hidden at 0 without dropping the
    // icon — `set_visible(false)` (not an empty string) keeps the button icon-only
    // until the post has activity.
    let count_label = gtk::Label::new(Some(&count.to_string()));
    count_label.add_css_class("caption");
    count_label.set_visible(count > 0);
    content.append(&count_label);

    content
}

/// Build one feed interaction button — a recognizable icon plus its interaction
/// count, **no word label**, with the count hidden when `count == 0` (a clean
/// icon-only button until the post has activity). This is the cross-app
/// ratified shape (`docs/goal/ui/feed.md` § Interaction bar, 2026-06-27): the
/// icon *glyph* is per-platform (Adwaita symbolic here, SF Symbols on apple),
/// but the icon+count+hidden-at-0 *shape* is uniform on all seven apps. The
/// action name rides the tooltip (the icon alone is not screen-reader-legible).
fn interaction_button(icon_name: &str, tooltip: &str, count: i64, test_id: &str) -> gtk::Button {
    let content = engagement_indicator_content(icon_name, count);
    let btn = gtk::Button::builder().child(&content).build();
    btn.add_css_class("flat");
    btn.set_tooltip_text(Some(tooltip));
    crate::testid::set_test_id(&btn, test_id);
    // The button's child is an image + a label, so `Button::label()` is empty
    // and the automation read would say nothing about what the button paints.
    // Declare it: the icon (by its name — the glyph itself carries no text)
    // and the count only while it is shown, so a read of a post with no
    // activity finds an icon and no number (`feed.md` § Interaction bar). The
    // button is rebuilt with its count on every render, so this never goes
    // stale against the child label's visibility.
    let painted = if count > 0 {
        format!("{icon_name} {count}")
    } else {
        icon_name.to_string()
    };
    crate::testid::set_test_text(&btn, &painted);
    btn
}

/// Build a post card row from a snapshot [`PostSummary`]. A post whose text
/// matches the user's muted words renders the collapse-to-placeholder arm
/// (topic-factors.md § Scoring — a mute collapses everywhere, chronological
/// feeds included; sinking is manager-internal and only where ranking
/// happens); everything else gets the full card body.
/// The post tip surface — `post-tip-total` / `post-tip-count` /
/// `post-tip-list-button` (`monetization.md` § Tips), shared by the card and
/// the detail view. `None` while `fire_resolves`-equivalent hasn't resolved
/// yet, and on an untipped post — one empty surface for both, the ratified
/// degradation.
///
/// **The two counters are guarded independently, and that is the whole
/// point.** `tip_count` counts every tip; `total_msats` sums only those whose
/// receipt reported an amount, so a post whose every receipt carried an
/// unparseable invoice renders the count and no total — rendering "0 sats"
/// there would tell the reader nobody paid (`monetization.md` § Tips: a
/// missing amount is a real state, never coerced to 0).
///
/// **Gated on the RENDER, not merely on the resolver that feeds it.**
/// `PostSummary.tips` is a deliberately ungated inert record, so in a
/// store-safe build this function would compile perfectly, return `None`
/// forever — and still ship `post-tip-total` / `post-tip-count` /
/// `post-tip-list-button` as string literals. Criterion 1 of
/// `dynamic-features.md` § What "completely compiled away" means is a
/// `strings`-grep for element ids: inertness makes the surface DEAD, it does
/// not make it ABSENT.
#[cfg(feature = "payments")]
pub fn build_tip_row(post: &PostSummary) -> Option<gtk::Box> {
    let post_tips = post.tips.as_ref()?;
    if post_tips.tip_count == 0 {
        return None;
    }
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_margin_top(4);

    if post_tips.total_msats != 0 {
        let total = gtk::Label::new(Some(&crate::i18n::tip_amount(post_tips.total_msats)));
        total.add_css_class("caption");
        total.add_css_class("dim-label");
        crate::testid::set_test_id(&total, ids::POST_TIP_TOTAL);
        row.append(&total);
    }

    let count = gtk::Label::new(Some(&crate::i18n::tip_count(post_tips.tip_count)));
    count.add_css_class("caption");
    count.add_css_class("dim-label");
    crate::testid::set_test_id(&count, ids::POST_TIP_COUNT);
    row.append(&count);

    let list_btn = gtk::Button::with_label(tips::LIST_OPEN);
    list_btn.add_css_class("flat");
    crate::testid::set_test_id(&list_btn, ids::POST_TIP_LIST_BUTTON);
    {
        let view = post_tips.clone();
        list_btn.connect_clicked(move |btn| {
            let parent = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
            show_tip_list_dialog(parent.as_ref(), &view);
        });
    }
    row.append(&list_btn);

    Some(row)
}

/// The `post-tip-list` attribution window for an already-resolved
/// [`TipView`] — `post-tip-list-button`'s target.
///
/// **Every row the nest sent is rendered, unfiltered.** A tip's authenticity
/// is settled at ingest and never at read (`monetization.md` § Zap
/// receipts), so a client-side trust check here would re-open exactly the
/// per-reader re-checking that discipline exists to prevent.
///
/// Gated for the same reason as [`build_tip_row`]: it emits `post-tip-list`
/// and `post-tip-item`, which a `strings`-grep sees whether or not the dialog
/// can ever open.
#[cfg(feature = "payments")]
fn show_tip_list_dialog(parent: Option<&gtk::Window>, post_tips: &TipView) {
    let window = adw::Window::builder()
        .title(tips::LIST_TITLE)
        .modal(true)
        .default_width(360)
        .build();
    if let Some(p) = parent {
        window.set_transient_for(Some(p));
        if let Some(app) = p.application() {
            window.set_application(Some(&app));
        }
    }

    let outer = gtk::Box::new(gtk::Orientation::Vertical, 8);
    outer.set_margin_top(16);
    outer.set_margin_bottom(16);
    outer.set_margin_start(16);
    outer.set_margin_end(16);
    crate::testid::set_test_id(&outer, ids::POST_TIP_LIST);

    // The bounded window's tail, from the nest's own `has_more` — never
    // inferred by comparing the rendered row count against a cap this client
    // hard-codes (the wire carries the flag precisely so no client has to).
    let title_text = if post_tips.has_more {
        let more = post_tips.tip_count - post_tips.senders.len() as i64;
        format!("{} — {}", tips::LIST_TITLE, crate::i18n::tip_more(more))
    } else {
        tips::LIST_TITLE.to_string()
    };
    let title = gtk::Label::new(Some(&title_text));
    title.add_css_class("heading");
    title.set_halign(gtk::Align::Start);
    outer.append(&title);

    for tip in &post_tips.senders {
        // Who: the local actor when the mechanism identity resolved to one,
        // else the mechanism-native id it published, else the localized
        // stand-in — an outside tip still counts and still displays.
        let who = tip
            .sender
            .as_deref()
            .or(tip.sender_ref.as_deref())
            .unwrap_or(tips::SENDER_UNKNOWN);
        // How much, or the honest absence. NEVER "0 sats".
        let amount = match tip.amount_msats {
            Some(msats) => crate::i18n::tip_amount(msats),
            None => tips::AMOUNT_UNKNOWN.to_string(),
        };
        let item = gtk::Label::new(Some(&format!("{who} — {amount}")));
        item.set_halign(gtk::Align::Start);
        item.set_wrap(true);
        crate::testid::set_test_id(&item, ids::POST_TIP_ITEM);
        outer.append(&item);
    }

    window.set_content(Some(&outer));
    window.present();
}

fn build_post_card(
    post: &PostSummary,
    manager: &Arc<LinuxFeedManager>,
    client: &Rc<FaunaClient>,
    rt: &Handle,
    error_label: &gtk::Label,
) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    // The scope test-id rides `widget_name` so the in-process automation agent
    // resolves `post-card` / `post-card[i]`; detail-view lookup + actions use
    // the row's *index* into `snapshot.posts` (rows are appended 1:1).
    row.set_widget_name("post-card");
    // The row carries its own post identity for the engagement-cue viewport
    // observer (`feed::viewport`): rows are destroyed + rebuilt on every
    // notify, and an index-into-snapshot read taken mid-rebuild can attribute
    // one post's dwell to another — the id must live ON the widget being
    // measured (the factor-key test-attr idiom).
    crate::testid::set_test_attr(&row, "post", &post.post_id);

    // Content-policy render enforcement (family-safety.md § Content policy): the
    // supervised viewer's guardian floor over this post's labels. A `block` floor
    // is absolute — no reveal — so it is checked FIRST, ahead of the muted-keyword
    // collapse, so a post that is both muted (revealable) and blocked can never be
    // revealed past the guardian's block. `collapse` is handled after the muted arm
    // (both are revealable collapses). Show/Badge/unsupervised → normal body below.
    let content_verdict = composed_verdict(post).verdict;
    // Guardian Notify (family-safety.md § Guardian Notify): count this post if the
    // guardian floor enforces on it — a no-op unless the ward's content_notify knob
    // is on. Deduped per post per local day; the app's flush tick reports the batch.
    crate::content_policy::note_enforcement(&post.post_id, &post.labels);
    // A REGION verdict paints the region's own placeholder — naming the region,
    // its authority and the authority's reason (region-blocking.md invariant 1)
    // — rather than the family notice; checked first because it is the same
    // verb, only better attributed. A revealed region `collapse` falls through
    // to the ordinary arms (tui's `feed/mod.rs` orders the arms the same way).
    if let Some(withheld) = region_withheld(post) {
        row.update_property(&[
            gtk::accessible::Property::Description("post-card"),
            gtk::accessible::Property::Label(&crate::region::notice_text(&withheld)),
        ]);
        let post_c = post.clone();
        let m = Arc::clone(manager);
        let c = Rc::clone(client);
        let rt_c = rt.clone();
        let error_label_c = error_label.clone();
        // Weak: the row owns this closure through its child.
        let row_weak = glib::object::ObjectExt::downgrade(&row);
        row.set_child(Some(&crate::region::placeholder_box(
            &withheld,
            move || {
                REVEALED_CONTENT_POSTS.with(|s| s.borrow_mut().insert(post_c.post_id.clone()));
                if let Some(row) = row_weak.upgrade() {
                    row.update_property(&[
                        gtk::accessible::Property::Description("post-card"),
                        gtk::accessible::Property::Label(&post_c.body),
                    ]);
                    row.set_child(Some(&build_post_card_body(
                        &post_c,
                        &m,
                        &c,
                        &rt_c,
                        &error_label_c,
                    )));
                }
            },
        )));
        return row;
    }
    if content_verdict == RenderVerdict::Block {
        row.update_property(&[
            gtk::accessible::Property::Description("post-card"),
            gtk::accessible::Property::Label(family::CONTENT_BLOCKED_NOTICE),
        ]);
        row.set_child(Some(&build_content_block()));
        return row;
    }

    // Muted-keyword collapse. `FeedManager::is_muted(post_id)` is the
    // canonical per-post signal (settled 2026-07-12, topic-factors.md
    // § Implementation status); the one-tap reveal is session-local render
    // state, mirroring `dm-message-muted`.
    let revealed = REVEALED_MUTED_POSTS.with(|s| s.borrow().contains(&post.post_id));
    if !revealed && manager.is_muted(&post.post_id) {
        // A11y label is the placeholder, never the matched body — the collapse
        // must not leak the text it hides into the accessibility tree.
        row.update_property(&[
            gtk::accessible::Property::Description("post-card"),
            gtk::accessible::Property::Label(feed::POST_MUTED_PLACEHOLDER),
        ]);
        row.set_child(Some(&build_muted_collapse(
            post,
            manager,
            client,
            rt,
            error_label,
            &row,
        )));
        return row;
    }

    // Content-policy `collapse` floor: render collapsed with a session-local
    // reveal (family-safety.md § Content policy — "rendered collapsed, reveal
    // affordance"), the same shape as the muted collapse but its own reveal set.
    let content_revealed = REVEALED_CONTENT_POSTS.with(|s| s.borrow().contains(&post.post_id));
    if !content_revealed && content_verdict == RenderVerdict::Collapse {
        row.update_property(&[
            gtk::accessible::Property::Description("post-card"),
            gtk::accessible::Property::Label(family::CONTENT_COLLAPSED_NOTICE),
        ]);
        row.set_child(Some(&build_content_collapse(
            post,
            manager,
            client,
            rt,
            error_label,
            &row,
        )));
        return row;
    }

    row.update_property(&[
        gtk::accessible::Property::Description("post-card"),
        gtk::accessible::Property::Label(&post.body),
    ]);
    row.set_child(Some(&build_post_card_body(
        post,
        manager,
        client,
        rt,
        error_label,
    )));
    row
}

/// The content-policy **block** placeholder — a policy-naming notice in place of
/// the body, **no reveal** (family-safety.md § Content policy; mirrors the
/// legal-takedown tombstone shape). A `block` verdict is always a guardian floor
/// (a viewer's own threshold only ever collapses). The `content-policy-blocked-notice`
/// element is the one ui.yaml ID this pillar renders (indexed, per post-card).
fn build_content_block() -> gtk::Box {
    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 4);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    let placeholder = gtk::Label::new(Some(family::CONTENT_BLOCKED_NOTICE));
    placeholder.add_css_class("dim-label");
    placeholder.set_halign(gtk::Align::Start);
    placeholder.set_wrap(true);
    placeholder.set_xalign(0.0);
    crate::testid::set_test_id(&placeholder, ids::CONTENT_POLICY_BLOCKED_NOTICE);
    vbox.append(&placeholder);
    vbox
}

/// The content-policy **collapse** placeholder + one-tap reveal (family-safety.md
/// § Content policy). Revealing rebuilds the full card body in place, session-local
/// (`REVEALED_CONTENT_POSTS`) — the floor itself persists; the guardian relaxing it
/// is what stops future collapse. The placeholder is presentation-only (no test id):
/// v1 e2e drives only the `block` case (the collapse reveal is a user affordance).
fn build_content_collapse(
    post: &PostSummary,
    manager: &Arc<LinuxFeedManager>,
    client: &Rc<FaunaClient>,
    rt: &Handle,
    error_label: &gtk::Label,
    row: &gtk::ListBoxRow,
) -> gtk::Box {
    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 4);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    let placeholder = gtk::Label::new(Some(family::CONTENT_COLLAPSED_NOTICE));
    placeholder.add_css_class("dim-label");
    placeholder.set_halign(gtk::Align::Start);
    placeholder.set_wrap(true);
    placeholder.set_xalign(0.0);
    vbox.append(&placeholder);

    let reveal = gtk::Button::with_label(family::CONTENT_REVEAL_BUTTON);
    reveal.add_css_class("flat");
    reveal.set_halign(gtk::Align::Start);
    {
        let post = post.clone();
        let m = Arc::clone(manager);
        let c = Rc::clone(client);
        let rt = rt.clone();
        let error_label = error_label.clone();
        // Weak: the row owns this button through its child (see build_muted_collapse).
        let row = glib::object::ObjectExt::downgrade(row);
        reveal.connect_clicked(move |_| {
            REVEALED_CONTENT_POSTS.with(|s| s.borrow_mut().insert(post.post_id.clone()));
            if let Some(row) = row.upgrade() {
                row.update_property(&[
                    gtk::accessible::Property::Description("post-card"),
                    gtk::accessible::Property::Label(&post.body),
                ]);
                row.set_child(Some(&build_post_card_body(
                    &post,
                    &m,
                    &c,
                    &rt,
                    &error_label,
                )));
            }
        });
    }
    vbox.append(&reveal);
    vbox
}

/// The muted-post placeholder + one-tap reveal. Revealing rebuilds the full
/// card body in place (session-local — the mute itself persists; un-muting the
/// word is what stops future collapse).
fn build_muted_collapse(
    post: &PostSummary,
    manager: &Arc<LinuxFeedManager>,
    client: &Rc<FaunaClient>,
    rt: &Handle,
    error_label: &gtk::Label,
    row: &gtk::ListBoxRow,
) -> gtk::Box {
    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 4);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    let placeholder = gtk::Label::new(Some(feed::POST_MUTED_PLACEHOLDER));
    placeholder.add_css_class("dim-label");
    placeholder.set_halign(gtk::Align::Start);
    crate::testid::set_test_id(&placeholder, ids::FEED_POST_MUTED);
    vbox.append(&placeholder);

    let reveal = gtk::Button::with_label(feed::POST_MUTED_REVEAL);
    reveal.add_css_class("flat");
    reveal.set_halign(gtk::Align::Start);
    crate::testid::set_test_id(&reveal, ids::FEED_POST_MUTED_REVEAL_BUTTON);
    {
        let post = post.clone();
        let m = Arc::clone(manager);
        let c = Rc::clone(client);
        let rt = rt.clone();
        let error_label = error_label.clone();
        // Weak: the row owns this button through its child, so a strong
        // capture would be a GObject reference cycle (rows rebuild every
        // notify; a cycle would leak one per muted card per render).
        let row = glib::object::ObjectExt::downgrade(row);
        reveal.connect_clicked(move |_| {
            REVEALED_MUTED_POSTS.with(|s| s.borrow_mut().insert(post.post_id.clone()));
            if let Some(row) = row.upgrade() {
                row.update_property(&[
                    gtk::accessible::Property::Description("post-card"),
                    gtk::accessible::Property::Label(&post.body),
                ]);
                row.set_child(Some(&build_post_card_body(
                    &post,
                    &m,
                    &c,
                    &rt,
                    &error_label,
                )));
            }
        });
    }
    vbox.append(&reveal);
    vbox
}

/// What the feed calls a post's author — the one shared resolver
/// (`value-formatting.md` § Peer display label) over the viewer's own nickname
/// for them (`contacts.md` § The private overlay). A **bridged** author's face
/// — the display name and handle the origin bridge served,
/// `PostSummary.author_display` (`bridges.md` § Unified feed ingestion →
/// *Bridged authors*) — fills the resolver's self-published-name and handle
/// slots, so the chain reads nickname → display name → handle → short id.
/// Card and detail both call this.
pub(crate) fn author_label_text(post: &PostSummary) -> String {
    author_label_with(&crate::conversations::overlays::projection(), post)
}

fn author_label_with(
    overlays: &fauna_conversations::contacts::ContactsCache,
    post: &PostSummary,
) -> String {
    let face = post.author_display.as_ref();
    overlays
        .peer_label(
            face.and_then(|f| f.display_name.as_deref()),
            face.and_then(|f| f.handle.as_deref()),
            &post.author,
        )
        .primary
}

/// Build the full post-card body: author + badges + metadata + body (markdown
/// blockquotes) + tag chips + the embedded quoted-post + media image + link
/// preview, the ⋯ overflow (training verbs), and Reply / Repost / Quote / Like
/// wired to `interact_with_post`, which now routes through the shared
/// `FeedManager::interact` so a tapped count moves at once.
fn build_post_card_body(
    post: &PostSummary,
    manager: &Arc<LinuxFeedManager>,
    client: &Rc<FaunaClient>,
    rt: &Handle,
    error_label: &gtk::Label,
) -> gtk::Box {
    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 4);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    // A REPOST ROW (feed.md § Interaction bar → Repost, ratified 2026-08-10):
    // attribution + the embedded original, no interaction bar of its own — the
    // repost post is empty by construction, so its own bar would be all zeros.
    let is_repost_row = post.reposted_post_id.is_some();

    // Top line: author + source badge + metadata badges + timestamp.
    let top_line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let author_label = gtk::Label::new(Some(&author_label_text(post)));
    author_label.set_halign(gtk::Align::Start);
    author_label.set_hexpand(true);
    author_label.add_css_class("heading");
    crate::testid::set_test_id(&author_label, ids::POST_AUTHOR);
    top_line.append(&author_label);

    // The attribution marker beside the author (id user-approved 2026-08-11):
    // what lets an e2e tell a repost card from an empty-commentary quote card
    // BY ELEMENT, instead of inferring it from the harness state dump.
    if is_repost_row {
        let attribution = gtk::Label::new(Some(&format!("⇄ {}", feed::post::REPOSTED_MARKER)));
        attribution.add_css_class("caption");
        attribution.add_css_class("dim-label");
        crate::testid::set_test_id(&attribution, ids::REPOST_ATTRIBUTION);
        top_line.append(&attribution);
    }

    for badge in build_protocol_badges(&post.source) {
        top_line.append(&badge);
    }

    if let Some(badge) = build_unverified_badge(post.verification) {
        top_line.append(&badge);
    }

    // The D10 audit marker (`delegated-origin-badge`): an external app wrote this
    // post as the account. Trails the unverified badge in the same top_line, the
    // order every app paints these two in.
    if let Some(badge) = build_delegated_origin_badge(post.authoring_origin) {
        top_line.append(&badge);
    }

    // Gated-to-tier badge (`gated-post-badge`, feed.md § Encryption at rest):
    // the tier name, on every gated post's card — the list body stays the
    // plaintext teaser; the detail unseals for entitled readers. A room post
    // this reader sits on the floor of names the room instead, by the
    // reader's own label (`PostSummary::room_label`, derived by the shared
    // manager; the composer's own "Room: ‹label›" string), and the card's
    // detail-open is its "open"; a reader not in the room sees the reserved
    // tier (feed.md § Encryption at rest → *the app half*, the card).
    if let Some(tier) = &post.gated_tier {
        let (text, tooltip) = match &post.room_label {
            Some(label) => (
                feed::post::gate_room(label),
                feed::post::gated_badge_room_tooltip(label),
            ),
            None => (tier.clone(), feed::post::gated_badge_tooltip(tier)),
        };
        let badge = gtk::Label::new(Some(&text));
        badge.add_css_class("caption");
        badge.add_css_class("dim-label");
        badge.set_tooltip_text(Some(&tooltip));
        crate::testid::set_test_id(&badge, ids::GATED_POST_BADGE);
        top_line.append(&badge);
    }

    // Buyer's price read (gap (2c), `monetization.md` § Per-post pay-to-unlock
    // → the buyer's price read is post-addressed) — resolved lazily off
    // `fauna.subscriptions.post_unlock.get` once `gated_tier` names a
    // `post-unlock-*` tier (`fire_resolves`, this page's resolve-trigger fn).
    // `None` covers both "not yet resolved" and "the nest answered no offer"
    // (a failed read included): both leave the priceless
    // teaser, with claim-code redemption (§5) as the fallback purchase path.
    if let Some(offer) = &post.unlock_offer {
        let price = gtk::Label::new(Some(offer.price_hint.as_deref().unwrap_or("")));
        price.add_css_class("caption");
        price.add_css_class("dim-label");
        crate::testid::set_test_id(&price, ids::GATED_POST_PRICE);
        top_line.append(&price);

        if let Some(url) = offer.payment_url.clone() {
            let pay = gtk::Button::with_label(subscriptions::PAYMENT_URL);
            pay.add_css_class("flat");
            crate::testid::set_test_id(&pay, ids::GATED_POST_PAYMENT_LINK);
            let error_label = error_label.clone();
            pay.connect_clicked(move |btn| {
                // The payment link is nest/author-supplied; refuse to open a
                // non-https scheme via the shared `fauna_core::subscription::
                // is_safe_payment_url` guard (F-CL2 anti-phishing-redirect
                // class — same check `views/profile/offers.rs`'s
                // subscription-offer-payment-link applies).
                if !fauna_core::subscription::is_safe_payment_url(&url) {
                    crate::settings::render_error_label(
                        &error_label,
                        Some(subscriptions::UNSAFE_PAYMENT_URL),
                    );
                    return;
                }
                gtk::UriLauncher::new(&url).launch(
                    btn.root().and_downcast::<gtk::Window>().as_ref(),
                    gtk::gio::Cancellable::NONE,
                    |_| {},
                );
            });
            top_line.append(&pay);
        }

        let buy = gtk::Button::with_label(feed::post::BUY_BUTTON);
        buy.add_css_class("flat");
        buy.add_css_class("suggested-action");
        crate::testid::set_test_id(&buy, ids::GATED_POST_BUY_BUTTON);
        crate::offline_gate::declare_wire_kind(&buy, "fauna.subscriptions.subscribe");
        {
            let m = Arc::clone(manager);
            let rt = rt.clone();
            let pid = post.post_id.clone();
            let error_label = error_label.clone();
            buy.connect_clicked(move |_| {
                let m = Arc::clone(&m);
                let pid = pid.clone();
                let error_label = error_label.clone();
                crate::async_helper::spawn_with_snapshot(
                    &rt,
                    move || async move { m.buy_unlock_offer(pid).await },
                    move |result: Option<Result<bool, String>>| {
                        if let Some(Err(msg)) = result {
                            crate::settings::render_error_label(
                                &error_label,
                                Some(&feed::error_buy_unlock(&msg)),
                            );
                        }
                    },
                );
            });
        }
        top_line.append(&buy);
    }

    // `content-label-badge` — the highest-confidence classifier verdict, if
    // any, presented entirely through the shared `content_label_style` map
    // (icon + accent colour + i18n label) — same idiom as the moderation
    // queue (`views/moderation.rs`) and the DM bubble, so no styling drifts
    // per surface (moderation.md § Where logic lives, drift #157).
    if let Some(entry) = fauna_core::content_category::primary_content_label(&post.labels) {
        top_line.append(&crate::views::moderation::build_content_label_badge(
            &entry.category,
        ));
    }

    if post.has_media {
        let media_icon = gtk::Image::from_icon_name("image-x-generic-symbolic");
        media_icon.set_pixel_size(14);
        media_icon.set_tooltip_text(Some(feed::post::HAS_MEDIA));
        top_line.append(&media_icon);
    }

    if post.is_reply {
        let reply_icon = gtk::Image::from_icon_name("mail-reply-sender-symbolic");
        reply_icon.set_pixel_size(14);
        reply_icon.set_tooltip_text(Some(common::REPLY));
        top_line.append(&reply_icon);
    }

    let time_label = gtk::Label::new(Some(&format_timestamp(post.timestamp)));
    time_label.set_halign(gtk::Align::End);
    time_label.add_css_class("dim-label");
    time_label.add_css_class("caption");
    top_line.append(&time_label);

    // Per-card ⋯ overflow (feed-post-actions-button → feed-post-actions-menu):
    // v1 hosts the trained-topic training verbs (topic-factors.md § Authoring
    // surface; the dm-message-actions-button precedent applied to posts).
    top_line.append(&build_post_actions_button(
        post,
        manager,
        client,
        rt,
        error_label,
    ));

    vbox.append(&top_line);

    // Body — walk the shared `RenderDocument` the feed manager already built from the
    // post body (`PostSummary.document`, render-model.md § D6), the SAME walker the
    // Conversations page uses. No per-render markdown parse, no bespoke `> ` blockquote
    // splitter; markdown structure (bold/italic/links/lists/headings/code/blockquote) is
    // honoured uniformly. Each remote `![]()` image paints from its authoritative
    // `revealed` flag (blocked placeholder or fetched picture — D3). The post's tags are
    // the snapshot's facet list, not parsed out of the body.
    let body_box = document::render_to_widget(
        &post.document,
        rt,
        &media_loads::MediaScope::Feed(Arc::clone(manager)),
    );
    crate::testid::set_test_id(&body_box, ids::FEED_POST_TEXT);
    vbox.append(&body_box);
    // The reveal button appears iff the document still has a blocked remote image, and
    // DISPATCHES `FeedManager::reveal_remote_images(post_id)` — the feed twin of the
    // conversations bubble (render-model.md § D3). The manager flips its reveal set + re-emits;
    // the feed observer rebuild re-renders this card with `revealed: true`, so the button is
    // simply absent next time. No client-side reveal state remains.
    if post.document.has_blocked_remote_images() {
        let m = Arc::clone(manager);
        let pid = post.post_id.clone();
        document::attach_reveal_button(&vbox, move || {
            m.reveal_remote_images(pid.clone());
        });
    }
    append_tag_chips(&vbox, &post.tags);

    // Embedded quoted post (`quoted-post`) + media image (`post-image`) paint
    // from the post `document` (render-model.md § D6): the walker above already
    // rendered the folded `QuotedPost` card inside `body_box`, and the resolved
    // media `Image` block is extracted here (the shared walker has no blob
    // loader) and painted through the client — the async-byte-load-stays-client
    // idiom. Both blocks are folded in lazily by `resolve_quoted_post` /
    // `resolve_media`, triggered fire-once in `render_posts`.
    // A bridged post's own picture / video (`ProxiedImage` / `ProxiedVideo`,
    // render-model.md § D6c) takes the same slot when there is no blob one.
    if let Some(hash) = post.document.first_image_hash() {
        vbox.append(&build_post_image(hash, manager, client));
    } else if let Some(path) = post.document.proxied_post_image() {
        vbox.append(&build_post_proxied_image(path, manager, client));
    }
    // `video-thumbnail` — the D6b `Video` sibling of `Image` (render-model.md
    // § D6b); mutually exclusive with the image branch above, same fold.
    if let Some(hash) = post.document.first_video_hash() {
        vbox.append(&build_post_video(hash));
    } else if let Some(path) = post.document.proxied_post_video() {
        vbox.append(&build_post_video(path));
    }

    // Link-preview cards (render-model.md § D4): one per Resolved `LinkPreview` block in
    // the post `document` (the producer emits one per standalone bare-url paragraph; the
    // manager resolves it to `Resolved` via `fauna.linkpreview.resolve`, fired fire-once in
    // `render_posts`). The og:image is extracted+painted here through the client's blob
    // loader and gated on its `revealed` flag — the same async-byte-load-stays-client +
    // D3-reveal idiom as the media image above. Resolving/Failed paint no card (the inline
    // body link already shows).
    for lp in post.document.resolved_link_previews() {
        vbox.append(&document::build_link_preview_card(
            &lp,
            client,
            &media_loads::MediaScope::Feed(Arc::clone(manager)),
        ));
    }

    // The tip surface (`monetization.md` § Tips) — resolved lazily by the
    // fire-once trigger below, absent until then and on an untipped post.
    // Painted on a repost row too (mirrors tui): the row's own tips are
    // whatever the empty wrapper post carries, which is empty by
    // construction, so `build_tip_row` returns `None` there regardless.
    #[cfg(feature = "payments")]
    if let Some(tip_row) = build_tip_row(post) {
        vbox.append(&tip_row);
    }

    // Bottom line: the interaction bar — like / reply / repost / quote, each an
    // icon + its `PostSummary` count (hidden at 0), no word labels, quote on all
    // six apps (feed.md § Interaction bar, ratified 2026-06-27). A REPOST
    // ROW renders no bar at all: its own counters are structurally dark, and
    // the original's live bar (with the toggle's `on` state) is one activation
    // away.
    if !is_repost_row {
        let bottom_line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        bottom_line.set_margin_top(4);

        let like_btn = interaction_button(
            "emblem-favorite-symbolic",
            feed::LIKE_TOOLTIP,
            post.like_count,
            "feed-like-button",
        );
        crate::offline_gate::declare_wire_kind(&like_btn, "fauna.posts.interact");
        // The like TOGGLE's lit state, off the `viewer_liked` projection (feed.md
        // § Interaction bar → Repost ratifies the carrier) — GTK's vocabulary for
        // tui's `state=on/off` attr. Without it a user cannot see that the next
        // tap will take the like back.
        if post.viewer_liked {
            like_btn.add_css_class("liked");
        }
        bottom_line.append(&like_btn);

        // Reply is purely local here: it opens `build_reply_dialog` below, and
        // the dialog's own submit (`ids::FEED_REPLY_SUBMIT_BUTTON`) is what
        // issues `interact_with_post(.., "reply", ..)` and carries its own
        // `declare_wire_kind`. This opener button itself issues nothing,
        // matching the arm/confirm split rule 2 uses for delete: gating the
        // opener would grey a door that works.
        let reply_btn = interaction_button(
            "mail-reply-sender-symbolic",
            common::REPLY,
            post.reply_count,
            "feed-reply-button",
        );
        bottom_line.append(&reply_btn);

        let repost_btn = interaction_button(
            "media-playlist-repeat-symbolic",
            feed::post::REPOST,
            post.repost_count,
            "feed-repost-button",
        );
        crate::offline_gate::declare_wire_kind(&repost_btn, "fauna.posts.interact");
        // The repost TOGGLE's lit state, off the `viewer_repost_id` projection
        // (feed.md § Interaction bar → Repost, ratified 2026-08-10) — the same
        // GTK vocabulary as the like toggle above.
        if post.viewer_repost_id.is_some() {
            repost_btn.add_css_class("reposted");
        }
        bottom_line.append(&repost_btn);

        let quote_btn = interaction_button(
            "mail-forward-symbolic",
            feed::QUOTE,
            post.quote_count,
            "feed-quote-button",
        );
        crate::offline_gate::declare_wire_kind(&quote_btn, "fauna.posts.interact");
        bottom_line.append(&quote_btn);

        vbox.append(&bottom_line);

        // Reply → compose dialog.
        {
            let c = Rc::clone(client);
            let pid = post.post_id.clone();
            reply_btn.connect_clicked(move |btn| {
                let dialog = build_reply_dialog(&c, &pid);
                if let Some(root) = btn.root()
                    && let Some(win) = root.downcast_ref::<gtk::Window>()
                {
                    dialog.set_transient_for(Some(win));
                }
                dialog.present();
            });
        }

        // Repost → the manager's TOGGLE off `viewer_repost_id`:
        // `interact_with_post` routes this to `FeedManager::repost`, which
        // composes the caller's empty-body `Reference::Repost` post (off → on)
        // or un-reposts it (on → off) — never a bare `interact`, which creates
        // nothing on a native post.
        {
            let c = Rc::clone(client);
            let pid = post.post_id.clone();
            repost_btn.connect_clicked(move |_| {
                c.interact_with_post(&pid, "repost", None);
            });
        }

        // Quote → immediate quote-repost with empty commentary (ui.yaml
        // `feed-quote-button`). `interact_with_post` routes this to the shared
        // `FeedManager::quote`, which COMPOSES a post carrying `Reference::Quote`;
        // the commentary composer is a fleet-wide follow-on, not a per-app
        // deviation.
        //
        // ⚠ This comment used to say linux "mirrors web's direct
        // `interact(post_id, 'quote')` call". That was a claim about another app
        // with no test behind it, and it was describing a call that creates
        // nothing on a native post — the exact rot class recorded in
        // `ui/feed.md` § Implementation status today. Don't reintroduce it.
        {
            let c = Rc::clone(client);
            let pid = post.post_id.clone();
            quote_btn.connect_clicked(move |_| {
                c.interact_with_post(&pid, "quote", None);
            });
        }

        // Like → immediate, and a TOGGLE: `interact_with_post` routes this to the
        // shared `FeedManager::like`, which reverses a live like through `unlike`
        // off the `viewer_liked` projection. The old direct one-way call could
        // never take a like back — the nest's like arm is idempotent per
        // (actor, post), so a second tap moved nothing on any app.
        {
            let c = Rc::clone(client);
            let pid = post.post_id.clone();
            like_btn.connect_clicked(move |_| {
                c.interact_with_post(&pid, "like", None);
            });
        }
    }

    vbox
}

/// The per-card ⋯ overflow: a flat icon button popping a `gtk::Popover` menu
/// (the `dm-message-actions-button` precedent applied to posts) hosting the
/// two training verbs. Train-in-context: with exactly one trained topic in the
/// effective composition (`FeedManager::train_target_factor`), a verb trains
/// that factor directly; otherwise it opens the factor-target sheet. The
/// rendered check state (`state` test-attr, `on`/`off` — the cross-app
/// toggle-read idiom) is the post's current example marker via
/// `example_label_for`; tapping the marked verb again un-trains (the inverse
/// delta), tapping the other verb re-trains (the manager applies the flip —
/// no UI-side duplicate guard, `TrainResult::DuplicateSignal` writes nothing).
fn build_post_actions_button(
    post: &PostSummary,
    manager: &Arc<LinuxFeedManager>,
    client: &Rc<FaunaClient>,
    rt: &Handle,
    error_label: &gtk::Label,
) -> gtk::Button {
    let btn = gtk::Button::from_icon_name("view-more-symbolic");
    btn.add_css_class("flat");
    crate::testid::set_test_id(&btn, ids::FEED_POST_ACTIONS_BUTTON);

    let popover = gtk::Popover::new();
    popover.set_autohide(true);
    popover.set_parent(&btn);
    let menu = gtk::Box::new(gtk::Orientation::Vertical, 6);
    menu.set_margin_top(6);
    menu.set_margin_bottom(6);
    menu.set_margin_start(6);
    menu.set_margin_end(6);
    crate::testid::set_test_id(&menu, ids::FEED_POST_ACTIONS_MENU);

    // The in-context target, resolved at card-build time (cards rebuild on
    // every manager notify, so this is snapshot-fresh).
    let target = manager.train_target_factor();

    for (verb, label, id) in [
        (
            TrainVerb::MoreLikeThis,
            feed::MORE_LIKE_THIS,
            "feed-post-more-like-this",
        ),
        (
            TrainVerb::LessLikeThis,
            feed::LESS_LIKE_THIS,
            "feed-post-less-like-this",
        ),
    ] {
        let item = gtk::Button::with_label(label);
        item.add_css_class("flat");
        crate::testid::set_test_id(&item, id);
        // Declared here, not only on the immediate-dispatch branch below: with
        // no in-context target this button opens `open_train_target_sheet`
        // instead (linux always builds both halves, unlike tui's early
        // return), but that sheet's own "Save" ends at the same
        // `dispatch_train` → `train_post`/`untrain_post` put. Same wire kind
        // either way, so one declaration on the button that starts the
        // ceremony covers it.
        crate::offline_gate::declare_wire_kind(&item, "fauna.personalization.model.put");
        let marked = target
            .as_deref()
            .map(|f| manager.example_label_for(&post.post_id, f) == Some(verb))
            .unwrap_or(false);
        crate::testid::set_test_attr(&item, "state", if marked { "on" } else { "off" });
        if marked {
            item.add_css_class("suggested-action");
        }
        {
            let m = Arc::clone(manager);
            let rt = rt.clone();
            let pid = post.post_id.clone();
            let target = target.clone();
            let pop = popover.clone();
            let error_label = error_label.clone();
            item.connect_clicked(move |it| {
                pop.popdown();
                match &target {
                    Some(factor) => dispatch_train(
                        &m,
                        &rt,
                        pid.clone(),
                        factor.clone(),
                        verb,
                        marked,
                        &error_label,
                    ),
                    None => open_train_target_sheet(it, &m, &rt, pid.clone(), verb, &error_label),
                }
            });
        }
        menu.append(&item);
    }

    // The own-post web-publishing verbs (`web-content-hosting.md`
    // § Published-post management; presence rules `ui/feed.md` § User
    // actions). Deliberately OUTSIDE the training loop above, which has no
    // early-return here to hide behind (unlike tui, whose target-sheet
    // fallback DOES early-return past this point — linux always builds both
    // halves).
    build_web_publish_verbs(&menu, &popover, post, client, manager, rt, error_label);

    // Own-post delete (feed.md § State & data shape → Post deletion, IDs
    // user-approved 2026-07-16) — a destructive two-step inside the same
    // flyout, mirroring conversations' dm-message-delete-button /
    // dm-message-delete-confirm-button verbatim.
    if client.actor_id().as_deref() == Some(post.author.as_str()) {
        let del = gtk::Button::with_label(feed::DELETE_POST);
        del.add_css_class("flat");
        del.add_css_class("destructive-action");
        crate::testid::set_test_id(&del, ids::FEED_POST_DELETE_BUTTON);

        let prompt = gtk::Label::new(Some(feed::DELETE_POST_CONFIRM_TITLE));
        prompt.add_css_class("dim-label");
        prompt.set_visible(false);

        let confirm = gtk::Button::with_label(feed::DELETE_POST_CONFIRM);
        confirm.add_css_class("destructive-action");
        confirm.set_visible(false);
        crate::testid::set_test_id(&confirm, ids::FEED_POST_DELETE_CONFIRM_BUTTON);
        // Only the CONFIRM declares — arming the step (`del`, above) issues
        // nothing, so gating it would grey a door to a verb that works.
        crate::offline_gate::declare_wire_kind(&confirm, "fauna.posts.delete");

        {
            let del2 = del.clone();
            let prompt = prompt.clone();
            let confirm2 = confirm.clone();
            del.connect_clicked(move |_| {
                del2.set_visible(false);
                prompt.set_visible(true);
                confirm2.set_visible(true);
            });
        }
        {
            let m = Arc::clone(manager);
            let rt = rt.clone();
            let pid = post.post_id.clone();
            let pop = popover.clone();
            let error_label = error_label.clone();
            confirm.connect_clicked(move |_| {
                pop.popdown();
                let m = Arc::clone(&m);
                let pid = pid.clone();
                let error_label = error_label.clone();
                crate::async_helper::spawn_with_snapshot(
                    &rt,
                    move || async move { m.delete_post(pid).await },
                    move |result| {
                        if let Err(msg) = result {
                            crate::settings::render_error_label(
                                &error_label,
                                Some(&feed::error_delete(&msg)),
                            );
                        }
                    },
                );
            });
        }
        menu.append(&del);
        menu.append(&prompt);
        menu.append(&confirm);
    }

    popover.set_child(Some(&menu));

    {
        let pop = popover.clone();
        btn.connect_clicked(move |_| pop.popup());
    }
    // A parented popover must be unparented when its button goes away, or GTK
    // warns "Finalizing … still has children left" on every row rebuild (rows
    // rebuild on each manager notify).
    {
        let pop = popover.clone();
        btn.connect_destroy(move |_| pop.unparent());
    }
    btn
}

/// The own-post web-publishing verbs (`web-content-hosting.md`
/// § Published-post management; presence rules `ui/feed.md` § User actions).
///
/// **Everything here is state-derived off the post the snapshot already
/// holds** — `PostSummary::{author, web_slug, gated_tier}` — never a per-row
/// query. `fauna.web.publish.set` is authorship-gated nest-side as well, so
/// this is not the security boundary — but a verb that can only ever refuse
/// is a verb that should never have painted.
///
/// **Both copy affordances disable when the actor has no serving origin**,
/// with the reason painted beside them: publishing with no origin is legal
/// but unreachable, and the doc is explicit that the UI must say so rather
/// than hand out a link that cannot load. The takedown stays live — it needs
/// no origin, and it is the one thing a user with an unreachable site may
/// well want.
fn build_web_publish_verbs(
    menu: &gtk::Box,
    popover: &gtk::Popover,
    post: &PostSummary,
    client: &Rc<FaunaClient>,
    manager: &Arc<LinuxFeedManager>,
    rt: &Handle,
    error_label: &gtk::Label,
) {
    if client.actor_id().as_deref() != Some(post.author.as_str()) {
        return;
    }

    let Some(slug) = post.web_slug.clone() else {
        // Unpublished: one verb, and no link affordances for a page that does
        // not exist. A default slug is the nest's to mint, so this needs no
        // origin and no input.
        let btn = gtk::Button::with_label(web_publish::PUBLISH_TO_WEB);
        btn.add_css_class("flat");
        crate::testid::set_test_id(&btn, ids::FEED_POST_PUBLISH_WEB_BUTTON);
        crate::offline_gate::declare_wire_kind(&btn, "fauna.web.publish.set");
        {
            let m = Arc::clone(manager);
            let c = Rc::clone(client);
            let rt = rt.clone();
            let pid = post.post_id.clone();
            let pop = popover.clone();
            let error_label = error_label.clone();
            btn.connect_clicked(move |_| {
                pop.popdown();
                dispatch_web_publish(&m, &c, &rt, pid.clone(), &error_label);
            });
        }
        menu.append(&btn);
        return;
    };

    // The origin the copy affordances build on — the same shared resolution
    // the web-settings section uses (active custom domain > enabled
    // subdomain), read from the cache that page hydrates
    // (`crate::settings::web`). An un-hydrated cache (nobody has opened
    // Settings → Web or another post's ⋯ menu this session) reads as "no
    // origin" — the same pessimistic-then-corrects flash tui's own lazy
    // hydrate accepts — and kicks off the ONE hydrate that repaints every
    // open card once it lands, rather than disabling the verb forever.
    let origin = crate::settings::web::cached_site_link().and_then(|l| l.origin);
    if origin.is_none() {
        let m = Arc::clone(manager);
        let rt2 = rt.clone();
        crate::settings::web::ensure_hydrated(client, move || {
            rt2.spawn(async move {
                m.refresh_current_feed().await;
            });
        });
    }

    // Said once for the menu rather than per verb: the ratified ~10-minute
    // validity and the claim code as the durable alternative. Only when the
    // paywall affordance is actually present below.
    if post.gated_tier.is_some() && origin.is_some() {
        menu.append(&chrome_label(web_publish::PAYWALL_LINK_NOTE));
    }
    // Why the copy verbs below are dead, in the user's own terms. The ⋯ menu
    // cannot say "the toggle above" — that control is on another page — so
    // the feed's own wording names where to go.
    if origin.is_none() {
        menu.append(&chrome_label(web_publish::MENU_NO_LINK_REASON));
    }

    let copy_btn = gtk::Button::with_label(web_publish::COPY_WEB_LINK);
    copy_btn.add_css_class("flat");
    crate::testid::set_test_id(&copy_btn, ids::FEED_POST_COPY_WEB_LINK_BUTTON);
    copy_btn.set_sensitive(origin.is_some());
    if let Some(origin) = &origin {
        let url = fauna_client_web::post_page_url(origin, &slug);
        copy_btn.connect_clicked(move |btn| {
            crate::clipboard::copy_text(&url);
            crate::testid::set_test_attr(btn, "copied", &url);
        });
    }
    menu.append(&copy_btn);

    // Gated rows only: an ungated post has no paywalled body to hand out, so
    // the mint would hand out a token for nothing.
    if post.gated_tier.is_some() {
        let paywall_btn = gtk::Button::with_label(web_publish::COPY_PAYWALL_LINK);
        paywall_btn.add_css_class("flat");
        crate::testid::set_test_id(&paywall_btn, ids::FEED_POST_COPY_PAYWALL_LINK_BUTTON);
        crate::offline_gate::declare_wire_kind(&paywall_btn, "fauna.web.paywall.mint_token");
        paywall_btn.set_sensitive(origin.is_some());
        if let Some(origin) = origin.clone() {
            let nest = client.nest_rpc().clone();
            let rt = rt.clone();
            let slug = slug.clone();
            let error_label = error_label.clone();
            paywall_btn.connect_clicked(move |btn| {
                dispatch_web_paywall_mint(
                    &nest,
                    &rt,
                    slug.clone(),
                    origin.clone(),
                    btn.clone(),
                    &error_label,
                );
            });
        }
        menu.append(&paywall_btn);
    }

    let unpublish_btn = gtk::Button::with_label(web_publish::UNPUBLISH);
    unpublish_btn.add_css_class("flat");
    crate::testid::set_test_id(&unpublish_btn, ids::FEED_POST_UNPUBLISH_WEB_BUTTON);
    crate::offline_gate::declare_wire_kind(&unpublish_btn, "fauna.web.publish.unset");
    {
        let m = Arc::clone(manager);
        let c = Rc::clone(client);
        let rt = rt.clone();
        let pid = post.post_id.clone();
        let pop = popover.clone();
        let error_label = error_label.clone();
        unpublish_btn.connect_clicked(move |_| {
            pop.popdown();
            dispatch_web_unpublish(&m, &c, &rt, pid.clone(), &error_label);
        });
    }
    menu.append(&unpublish_btn);
}

/// A plain dim-label line — the GTK analogue of `Element::chrome`: visible
/// explanatory text with no test id, since nothing reads it as an element.
fn chrome_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_halign(gtk::Align::Start);
    label.set_wrap(true);
    label.add_css_class("dim-label");
    label
}

/// A snapshot post id (hex) as the wire's `post_id` bytes. Fallible on purpose
/// rather than a silent default: publishing against an undecodable id would
/// ask the nest to serve a post that does not exist.
fn decode_post_id(post_id: &str) -> Result<Vec<u8>, String> {
    hex::decode(post_id).map_err(|e| format!("unreadable post id {post_id:?}: {e}"))
}

/// `fauna.web.publish.set` for an own unpublished post (`None` slug ⇒ the
/// nest's default), then a re-read of the feed so the card's `web_slug` — and
/// with it the whole verb family — repaints from the nest's own answer rather
/// than optimistically. Both calls run in the SAME async step so the render
/// callback only ever has to handle the error case.
fn dispatch_web_publish(
    manager: &Arc<LinuxFeedManager>,
    client: &Rc<FaunaClient>,
    rt: &Handle,
    post_id: String,
    error_label: &gtk::Label,
) {
    let m = Arc::clone(manager);
    let nest = client.nest_rpc().clone();
    let error_label = error_label.clone();
    crate::async_helper::spawn_with_snapshot(
        rt,
        move || async move {
            let bytes = decode_post_id(&post_id)?;
            match fauna_client_web::WebClient::new(nest)
                .publish_set(bytes, None)
                .await
            {
                Ok(_) => {
                    m.refresh_current_feed().await;
                    Ok(())
                }
                Err(e) => Err(web_publish::error_publish(&e.to_string())),
            }
        },
        move |result: Result<(), String>| {
            if let Err(msg) = result {
                crate::settings::render_error_label(&error_label, Some(&msg));
            }
        },
    );
}

/// `fauna.web.publish.unset`, then the same re-read as publish — idempotent
/// nest-side, which is what lets the verb be one tap with no confirm step.
fn dispatch_web_unpublish(
    manager: &Arc<LinuxFeedManager>,
    client: &Rc<FaunaClient>,
    rt: &Handle,
    post_id: String,
    error_label: &gtk::Label,
) {
    let m = Arc::clone(manager);
    let nest = client.nest_rpc().clone();
    let error_label = error_label.clone();
    crate::async_helper::spawn_with_snapshot(
        rt,
        move || async move {
            let bytes = decode_post_id(&post_id)?;
            match fauna_client_web::WebClient::new(nest)
                .publish_unset(bytes)
                .await
            {
                Ok(_) => {
                    m.refresh_current_feed().await;
                    Ok(())
                }
                Err(e) => Err(web_publish::error_unpublish(&e.to_string())),
            }
        },
        move |result: Result<(), String>| {
            if let Err(msg) = result {
                crate::settings::render_error_label(&error_label, Some(&msg));
            }
        },
    );
}

/// `fauna.web.paywall.mint_token` for a published+gated own post — a fresh
/// mint per click, since the token is short-lived by ratified design and
/// re-minting is free (`monetization.md` § Pillar 2 → Creator comp-link
/// surface).
fn dispatch_web_paywall_mint(
    nest: &Arc<NestClient>,
    rt: &Handle,
    slug: String,
    origin: String,
    btn: gtk::Button,
    error_label: &gtk::Label,
) {
    let nest = Arc::clone(nest);
    let error_label = error_label.clone();
    crate::async_helper::spawn_with_snapshot(
        rt,
        move || async move {
            fauna_client_web::WebClient::new(nest)
                .paywall_mint_token(fauna_client_web::PaywallTarget::PostSlug { slug })
                .await
                .map(|minted| fauna_client_web::tokened_url(&origin, &minted.path, &minted.token))
                .map_err(|e| web_publish::error_paywall_link(&e.to_string()))
        },
        move |result: Result<String, String>| match result {
            Ok(url) => {
                crate::clipboard::copy_text(&url);
                crate::testid::set_test_attr(&btn, "copied", &url);
                crate::settings::render_error_label(&error_label, None);
            }
            Err(msg) => crate::settings::render_error_label(&error_label, Some(&msg)),
        },
    );
}

/// Run a training gesture on the tokio runtime: the marked verb again ⇒
/// un-train (the inverse delta), anything else ⇒ train (the manager applies
/// forward / flip semantics). An `Err` resolves through `feed::error_train`
/// into the page `error-message` (the manager's snapshot error only carries
/// its own load-path failures).
fn dispatch_train(
    manager: &Arc<LinuxFeedManager>,
    rt: &Handle,
    post_id: String,
    factor: String,
    verb: TrainVerb,
    already_marked: bool,
    error_label: &gtk::Label,
) {
    let m = Arc::clone(manager);
    let error_label = error_label.clone();
    crate::async_helper::spawn_with_snapshot(
        rt,
        move || async move {
            if already_marked {
                m.untrain_post(post_id, factor).await.map(|_| ())
            } else {
                m.train_post(post_id, factor, verb).await.map(|_| ())
            }
        },
        move |result: Result<(), String>| {
            if let Err(msg) = result {
                crate::settings::render_error_label(&error_label, Some(&feed::error_train(&msg)));
            }
        },
    );
}

/// The factor-target sheet (`feed-post-train-target-sheet`): shown when the
/// current feed's composition does not single out one trained topic. Lists the
/// user's trained factors from the sealed registry — model strings are the
/// stable `topic:<hex>` keys, the expression renders the user's chosen name
/// (the feed-factor-select pattern) — and trains the picked one.
fn open_train_target_sheet(
    anchor: &impl IsA<gtk::Widget>,
    manager: &Arc<LinuxFeedManager>,
    rt: &Handle,
    post_id: String,
    verb: TrainVerb,
    error_label: &gtk::Label,
) {
    let toplevel = anchor
        .as_ref()
        .root()
        .and_then(|r| r.downcast::<gtk::Window>().ok());
    // adw::MessageDialog — not yet migrated to AlertDialog (v1_5 has been
    // enabled since 2026-05-31); outside crate::confirm_dialog's
    // seam since this isn't a destructive confirm.
    #[allow(deprecated)]
    let dialog = adw::MessageDialog::new(toplevel.as_ref(), Some(feed::TRAIN_TARGET_TITLE), None);

    let factor_keys: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let factor_names: Rc<RefCell<std::collections::HashMap<String, String>>> =
        Rc::new(RefCell::new(std::collections::HashMap::new()));
    let model = gtk::StringList::new(&[]);
    let dropdown = gtk::DropDown::new(Some(model.clone()), gtk::Expression::NONE);
    {
        let factor_names = Rc::clone(&factor_names);
        let label_expr = gtk::ClosureExpression::new::<String>(
            &[] as &[gtk::Expression],
            glib::closure_local!(move |item: gtk::StringObject| {
                let key = item.string().to_string();
                factor_names.borrow().get(&key).cloned().unwrap_or(key)
            }),
        );
        dropdown.set_expression(Some(&label_expr));
    }

    let sheet = gtk::Box::new(gtk::Orientation::Vertical, 8);
    crate::testid::set_test_id(&sheet, ids::FEED_POST_TRAIN_TARGET_SHEET);
    sheet.append(&dropdown);
    dialog.set_extra_child(Some(&sheet));

    // Populate from the sealed registry — async on the tokio runtime, results
    // applied on the GTK thread (spawn_with_snapshot; a glib-context await of
    // a NestClient call panics — its reply timeout is a tokio timer). The
    // sheet opens immediately and the options land when the read resolves.
    {
        let factor_keys = Rc::clone(&factor_keys);
        let factor_names = Rc::clone(&factor_names);
        let model = model.clone();
        crate::async_helper::spawn_with_snapshot(
            rt,
            move || async move {
                match fauna_sync_engine::preference_surfaces::load_personalization(
                    &crate::account_runtime::handle_source(),
                )
                .await
                {
                    Ok(registry) => registry
                        .trained_factors
                        .iter()
                        .filter_map(|m| m.factor_key().map(|k| (k, m.name.clone())))
                        .collect(),
                    Err(_) => Vec::new(),
                }
            },
            move |topics: Vec<(String, String)>| {
                for (key, name) in topics {
                    factor_names.borrow_mut().insert(key.clone(), name);
                    model.append(&key);
                    factor_keys.borrow_mut().push(key);
                }
            },
        );
    }

    dialog.add_response("cancel", common::CANCEL);
    dialog.add_response("train", common::SAVE);
    dialog.set_response_appearance("train", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("train"));
    dialog.set_close_response("cancel");

    {
        let m = Arc::clone(manager);
        let rt = rt.clone();
        let error_label = error_label.clone();
        dialog.connect_response(None, move |dlg, response| {
            if response == "train" {
                let idx = dropdown.selected() as usize;
                if let Some(factor) = factor_keys.borrow().get(idx).cloned() {
                    dispatch_train(&m, &rt, post_id.clone(), factor, verb, false, &error_label);
                }
            }
            dlg.close();
        });
    }
    dialog.present();
}

/// Render the epoch-millis `timestamp` as a relative-time string via the shared
/// [`crate::client::format_epoch_us`] (the same "Nm/Nh/Nd ago" formatter search
/// / media / backups use — priority #2/#3). `format_epoch_us` takes
/// **microseconds**, so the snapshot's millis are scaled back up; a `0`
/// timestamp renders empty (the formatter's "no timestamp" sentinel).
pub(crate) fn format_timestamp(millis: i64) -> String {
    crate::client::format_epoch_us(millis.saturating_mul(1000))
}

/// Build a reply compose dialog (the send goes through `interact_with_post`
/// → the shared `FeedManager::interact`).
pub fn build_reply_dialog(client: &Rc<FaunaClient>, post_id: &str) -> adw::Window {
    let dialog = adw::Window::builder()
        .title(common::REPLY)
        .modal(true)
        .default_width(400)
        .build();
    // The first automation ids for this surface (`ui.yaml`'s
    // `feed-reply-dialog` note: "linux's build_reply_dialog … registers no
    // automation ids of its own — these IDs are the first for this surface").
    crate::testid::set_test_id(&dialog, ids::FEED_REPLY_DIALOG);

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 12);
    vbox.set_margin_top(16);
    vbox.set_margin_bottom(16);
    vbox.set_margin_start(16);
    vbox.set_margin_end(16);

    let text_view = gtk::TextView::new();
    text_view.set_wrap_mode(gtk::WrapMode::Word);
    text_view.set_top_margin(8);
    text_view.set_bottom_margin(8);
    text_view.set_left_margin(8);
    text_view.set_right_margin(8);
    crate::testid::set_test_id(&text_view, ids::FEED_REPLY_TEXT_FIELD);
    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .min_content_height(80)
        .max_content_height(240)
        .propagate_natural_height(true)
        .child(&text_view)
        .build();

    let reply_btn = gtk::Button::with_label(common::REPLY);
    reply_btn.add_css_class("suggested-action");
    // The dialog's own submit — this is what actually issues
    // `interact_with_post(.., "reply", ..)` below (routed to the shared
    // `FeedManager::reply`/`compose_referencing_post`, same "fauna.posts.interact"
    // declared kind the outer bar's quote/like/repost buttons use for the same
    // manager-routed call shape). NOT `ids::FEED_REPLY_BUTTON` — that id is
    // already on the post-card button that opens this dialog (`interaction_button`
    // call above); this is a DIFFERENT, already-registered id for the dialog's
    // real mutating control.
    crate::testid::set_test_id(&reply_btn, ids::FEED_REPLY_SUBMIT_BUTTON);
    crate::offline_gate::declare_wire_kind(&reply_btn, "fauna.posts.interact");

    vbox.append(&scrolled);
    vbox.append(&reply_btn);
    dialog.set_content(Some(&vbox));

    {
        let c = Rc::clone(client);
        let pid = post_id.to_string();
        let d = dialog.clone();
        let tv = text_view.clone();
        reply_btn.connect_clicked(move |_| {
            let buffer = tv.buffer();
            let start = buffer.start_iter();
            let end = buffer.end_iter();
            let text = buffer.text(&start, &end, false).to_string();
            if !text.trim().is_empty() {
                c.interact_with_post(&pid, "reply", Some(&text));
                d.close();
            }
        });
    }

    dialog
}

/// Build a rich compose dialog for new posts, submitting via
/// `FeedManager::submit_post`.
fn build_feed_compose_dialog(
    manager: &Arc<LinuxFeedManager>,
    client: &Rc<FaunaClient>,
) -> adw::Window {
    let dialog = adw::Window::builder()
        .title(composer::NEW_POST)
        .modal(true)
        .default_width(480)
        .default_height(320)
        .build();
    crate::testid::set_test_id(&dialog, ids::FEED_COMPOSE_DIALOG);

    let header = adw::HeaderBar::new();
    let close_btn = gtk::Button::from_icon_name("window-close-symbolic");
    close_btn.add_css_class("flat");
    {
        let d = dialog.clone();
        close_btn.connect_clicked(move |_| {
            d.close();
        });
    }
    header.pack_start(&close_btn);

    let text_view = gtk::TextView::new();
    text_view.set_wrap_mode(gtk::WrapMode::Word);
    text_view.set_top_margin(8);
    text_view.set_bottom_margin(8);
    text_view.set_left_margin(8);
    text_view.set_right_margin(8);
    text_view.set_vexpand(true);
    crate::testid::set_test_id(&text_view, ids::COMPOSE_TEXT_FIELD);

    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .child(&text_view)
        .hexpand(true)
        .vexpand(true)
        .build();

    let file_ready_marker = gtk::Label::new(None);
    file_ready_marker.add_css_class("caption");
    crate::testid::set_test_id(&file_ready_marker, ids::COMPOSE_FILE_READY);

    let file_remove_btn = gtk::Button::from_icon_name("window-close-symbolic");
    file_remove_btn.add_css_class("flat");
    crate::testid::set_test_id(&file_remove_btn, ids::COMPOSE_FILE_REMOVE);

    // Initial state off the manager's own `attached_file` — a restored
    // draft's handle must be visible (and droppable) the moment the dialog
    // opens, not only after a fresh local pick (`ui/feed.md` § Persistence →
    // *Attachments by content address*). This dialog is a one-shot form with
    // no observer loop of its own (unlike the inline bar's `render_posts`),
    // so the attach/remove handlers below re-run this same push imperatively.
    apply_compose_file_chip(
        &file_ready_marker,
        &file_remove_btn,
        manager.snapshot().compose.attached_file.as_ref(),
    );

    let attach_btn = gtk::Button::from_icon_name("mail-attachment-symbolic");
    attach_btn.set_tooltip_text(Some(feed::post::ATTACH_IMAGE));
    attach_btn.add_css_class("flat");
    attach_btn.set_valign(gtk::Align::Center);
    attach_btn.set_margin_start(4);
    crate::testid::set_test_id(&attach_btn, ids::COMPOSE_FILE);

    {
        let c = Rc::clone(client);
        let m = Arc::clone(manager);
        let ready = file_ready_marker.clone();
        let remove = file_remove_btn.clone();
        attach_btn.connect_clicked(move |btn| {
            let file_dialog = gtk::FileDialog::builder()
                .title(feed::post::ATTACH_IMAGE)
                .build();

            let filter = gtk::FileFilter::new();
            filter.set_name(Some("Images (*.png, *.jpg, *.webp, *.gif)"));
            filter.add_mime_type("image/png");
            filter.add_mime_type("image/jpeg");
            filter.add_mime_type("image/webp");
            filter.add_mime_type("image/gif");
            let filters = gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);
            file_dialog.set_filters(Some(&filters));
            file_dialog.set_default_filter(Some(&filter));

            let win = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
            let client_for_upload = Rc::clone(&c);
            let m = Arc::clone(&m);
            let ready = ready.clone();
            let remove = remove.clone();
            file_dialog.open(win.as_ref(), gio::Cancellable::NONE, move |result| {
                if let Ok(file) = result
                    && let Some(path) = file.path()
                {
                    let path_str = path.to_string_lossy().to_string();
                    client_for_upload.stage_attachment(&path_str);
                    // `stage_attachment` already staged the resolved handle
                    // onto the manager — read it back rather than
                    // re-deriving name/size here, so the dialog can never
                    // show a different answer than a submit would refuse or
                    // accept.
                    let attached = m.snapshot().compose.attached_file;
                    apply_compose_file_chip(&ready, &remove, attached.as_ref());
                }
            });
        });
    }

    {
        let m = Arc::clone(manager);
        let c = Rc::clone(client);
        let ready = file_ready_marker.clone();
        let remove = file_remove_btn.clone();
        file_remove_btn.connect_clicked(move |_| {
            c.clear_staged_attachment();
            let snap = m.snapshot();
            m.update_compose(snap.compose.text, snap.compose.tags, None);
            apply_compose_file_chip(&ready, &remove, None);
        });
    }

    let tags_entry = gtk::Entry::new();
    tags_entry.set_placeholder_text(Some(feed::post::TAGS_PLACEHOLDER));
    crate::testid::set_test_id(&tags_entry, ids::COMPOSE_TAGS_FIELD);

    let post_btn = gtk::Button::with_label(composer::NEW_POST);
    post_btn.add_css_class("suggested-action");
    post_btn.set_valign(gtk::Align::Center);
    post_btn.set_margin_start(8);
    crate::testid::set_test_id(&post_btn, ids::POST_SUBMIT_BUTTON);
    crate::offline_gate::declare_wire_kind(&post_btn, "fauna.posts.create");

    {
        let m = Arc::clone(manager);
        let c = Rc::clone(client);
        let tv = text_view.clone();
        let te = tags_entry.clone();
        let d = dialog.clone();
        let ready = file_ready_marker.clone();
        let remove = file_remove_btn.clone();
        post_btn.connect_clicked(move |_| {
            let buffer = tv.buffer();
            let start = buffer.start_iter();
            let end = buffer.end_iter();
            let text = buffer.text(&start, &end, false).to_string();
            let tags = te.text().to_string();
            if text.trim().is_empty() {
                return;
            }
            // The dialog carries no audience controls of its own; it posts
            // the audience the inline composer staged — the manager's, which
            // `submit_post` reads itself.
            c.submit_post(Arc::clone(&m), text, tags);
            apply_compose_file_chip(&ready, &remove, None);
            d.close();
        });
    }

    let error_label = gtk::Label::new(None);
    error_label.set_visible(false);
    error_label.set_wrap(true);
    error_label.add_css_class("error");
    crate::testid::set_test_id(&error_label, ids::COMPOSE_ERROR);

    let action_bar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    action_bar.set_margin_top(4);
    action_bar.set_margin_bottom(8);
    action_bar.set_margin_start(8);
    action_bar.set_margin_end(8);
    action_bar.append(&attach_btn);
    action_bar.append(&file_ready_marker);
    action_bar.append(&file_remove_btn);
    action_bar.append(&tags_entry);
    tags_entry.set_hexpand(true);
    action_bar.append(&post_btn);

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
    vbox.append(&header);
    vbox.append(&scroll);
    vbox.append(&action_bar);
    vbox.append(&error_label);
    dialog.set_content(Some(&vbox));

    dialog
}

/// Render a post's facet tags (`PostSummary.tags`) as `tag-chip`s. Each chip is
/// shown with a leading `#` (matching the other apps).
pub(crate) fn append_tag_chips(container: &gtk::Box, tags: &[String]) {
    if tags.is_empty() {
        return;
    }
    let tags_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    tags_box.set_margin_top(4);
    tags_box.set_halign(gtk::Align::Start);
    for tag in tags {
        let display = if tag.starts_with('#') {
            tag.clone()
        } else {
            format!("#{tag}")
        };
        let chip = gtk::Label::new(Some(&display));
        chip.add_css_class("tag");
        crate::testid::set_test_id(&chip, ids::TAG_CHIP);
        tags_box.append(&chip);
    }
    container.append(&tags_box);
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::first_label;

    fn room(hex: &str, label: &str) -> fauna_feed::GateRoomOption {
        fauna_feed::GateRoomOption {
            room: hex.to_string(),
            label: label.to_string(),
        }
    }

    /// The audience select's positions: Public, tiers, rooms, then Sell last —
    /// tui's order — and a room answer carries the room's channel id, never
    /// its label (`ui/feed.md` § Encryption at rest → *Room-restricted — the
    /// app half*).
    #[test]
    fn gate_options_map_each_position_to_its_answer() {
        let options = GateOptions {
            tiers: vec!["gold".into()],
            rooms: vec![room("aa", "Book club"), room("bb", "Crew")],
        };
        assert_eq!(
            options.labels(),
            vec![
                feed::post::GATE_PUBLIC.to_string(),
                "gold".to_string(),
                feed::post::gate_room("Book club"),
                feed::post::gate_room("Crew"),
                feed::post::GATE_SELL.to_string(),
            ]
        );
        assert_eq!(options.answer(0), GateAnswer::Public);
        assert_eq!(
            options.answer(gtk::INVALID_LIST_POSITION),
            GateAnswer::Public
        );
        assert_eq!(options.answer(1), GateAnswer::Tier("gold".into()));
        assert_eq!(options.answer(2), GateAnswer::Room("aa".into()));
        assert_eq!(options.answer(3), GateAnswer::Room("bb".into()));
        assert_eq!(options.answer(4), GateAnswer::Sell);
        assert_eq!(options.answer(5), GateAnswer::Public);
    }

    /// A tier named exactly a room option's label is still the tier: answers
    /// come from positions, so a name cannot hijack another answer.
    #[test]
    fn gate_options_keep_a_tier_named_like_a_room_option_a_tier() {
        let label = feed::post::gate_room("Crew");
        let options = GateOptions {
            tiers: vec![label.clone()],
            rooms: vec![room("bb", "Crew")],
        };
        assert_eq!(options.answer(1), GateAnswer::Tier(label));
        assert_eq!(options.answer(2), GateAnswer::Room("bb".into()));
    }

    /// A rebuilt list keeps the in-progress answer at its new position — a room
    /// joined mid-compose shifts Sell and the later rooms — and falls back to
    /// Public for an answer the new list no longer offers.
    #[test]
    fn gate_options_rebuilt_keep_the_answer_or_fall_back_to_public() {
        let before = GateOptions {
            tiers: vec![],
            rooms: vec![room("bb", "Crew")],
        };
        let after = GateOptions {
            tiers: vec![],
            rooms: vec![room("aa", "Book club"), room("bb", "Crew")],
        };
        assert_eq!(after.index_of(&before.answer(1)), 2);
        assert_eq!(
            after.index_of(&before.answer(2)),
            3,
            "Sell moves past the new room"
        );
        let gone = GateOptions::default();
        assert_eq!(gone.index_of(&before.answer(1)), 0);
    }

    /// An interaction button shows its count and carries its ui.yaml test-id when
    /// the count is non-zero (feed.md § Interaction bar — icon + count).
    #[test]
    fn interaction_button_shows_count_when_nonzero() {
        crate::testid::run_on_gtk_thread(|| {
            let btn = interaction_button("emblem-favorite-symbolic", "Like", 5, "feed-like-button");
            assert_eq!(btn.widget_name(), "feed-like-button");
            let label = first_label(&btn).expect("count label present");
            assert_eq!(label.text(), "5");
            assert!(
                label.property::<bool>("visible"),
                "count label visible when count > 0",
            );
        });
    }

    /// A re-query of the selection a detail was opened under keeps the detail —
    /// the feed page's own map refresh after a search deep link used to wipe it
    /// whenever the deep link's resolve had already painted.
    #[test]
    fn an_open_detail_outlives_a_re_query_of_the_same_selection() {
        let local: FeedSelection = (None, false);
        let custom: FeedSelection = (Some("aa".into()), false);
        let trending: FeedSelection = (None, true);
        for selection in [&local, &custom, &trending] {
            assert!(detail_outlives_render(Some(selection), selection));
        }
        assert!(
            detail_outlives_render(None, &custom),
            "a detail opened before any page settled has no selection to have moved off"
        );
    }

    /// Picking another feed, Trending, or back to the local feed drops the
    /// detail — and Trending is its own selection, not the local feed's
    /// (`selected_feed` is `None` for both).
    #[test]
    fn an_open_detail_is_dropped_when_the_selection_moves() {
        let local: FeedSelection = (None, false);
        let custom: FeedSelection = (Some("aa".into()), false);
        let other: FeedSelection = (Some("bb".into()), false);
        let trending: FeedSelection = (None, true);
        assert!(!detail_outlives_render(Some(&local), &custom));
        assert!(!detail_outlives_render(Some(&custom), &other));
        assert!(!detail_outlives_render(Some(&custom), &trending));
        assert!(!detail_outlives_render(Some(&trending), &local));
    }

    /// The count is hidden at 0 — a clean icon-only button until the post has
    /// activity (ratified 2026-06-27).
    #[test]
    fn interaction_button_hides_count_at_zero() {
        crate::testid::run_on_gtk_thread(|| {
            let btn = interaction_button("emblem-favorite-symbolic", "Like", 0, "feed-like-button");
            let label = first_label(&btn).expect("count label present");
            assert!(
                !label.property::<bool>("visible"),
                "count label hidden when count == 0",
            );
        });
    }

    // ── post-image repaint cost (`crate::media_loads`) ──────────────────────

    /// The reply end of one blob GET a card issued.
    type BlobReply = async_channel::Sender<Result<Vec<u8>, ApiError>>;
    /// The reply end of one C2PA check a card issued.
    type CheckReply = async_channel::Sender<Result<bool, ApiError>>;

    /// A fake nest for `build_post_image_from`: it records every blob GET and
    /// C2PA check a card issues and holds each reply until the test sends it.
    #[derive(Default)]
    struct CountingMedia {
        gets: RefCell<Vec<BlobReply>>,
        checks: RefCell<Vec<CheckReply>>,
        /// Every proxied-path GET, with the path it asked for.
        proxied: RefCell<Vec<(String, BlobReply)>>,
    }

    impl document::BlobSource for CountingMedia {
        fn fetch_blob_bytes(
            &self,
            _hash: &str,
        ) -> async_channel::Receiver<Result<Vec<u8>, ApiError>> {
            let (tx, rx) = async_channel::bounded(1);
            self.gets.borrow_mut().push(tx);
            rx
        }
    }

    impl PostMediaSource for CountingMedia {
        fn fetch_has_c2pa(&self, _hash: &str) -> async_channel::Receiver<Result<bool, ApiError>> {
            let (tx, rx) = async_channel::bounded(1);
            self.checks.borrow_mut().push(tx);
            rx
        }

        fn fetch_proxied_bytes(
            &self,
            path: &str,
        ) -> async_channel::Receiver<Result<Vec<u8>, ApiError>> {
            let (tx, rx) = async_channel::bounded(1);
            self.proxied.borrow_mut().push((path.to_string(), tx));
            rx
        }
    }

    impl CountingMedia {
        /// `(blob GETs, C2PA checks)` issued so far.
        fn counts(&self) -> (usize, usize) {
            (self.gets.borrow().len(), self.checks.borrow().len())
        }

        /// Answer the `n`th GET and the `n`th check.
        fn answer(&self, n: usize, bytes: &[u8], has_c2pa: bool) {
            let _ = self.gets.borrow()[n].try_send(Ok(bytes.to_vec()));
            let _ = self.checks.borrow()[n].try_send(Ok(has_c2pa));
        }
    }

    fn png() -> Vec<u8> {
        fauna_media::test_fixtures::build_png(2, 2)
    }

    fn offline_manager() -> Arc<LinuxFeedManager> {
        Arc::new(fauna_feed::FeedManager::new(
            NestClient::new(
                "http://127.0.0.1:1".to_string(),
                fauna_core::identity::ActorKeypair::generate(),
            ),
            [7u8; 32],
        ))
    }

    /// A card's `post-image` picture and its `c2pa-badge`, by
    /// `build_post_image_from`'s own layout.
    fn image_parts(card: &gtk::Box) -> (gtk::Picture, gtk::Label) {
        let button = card
            .first_child()
            .and_downcast::<gtk::Button>()
            .expect("the post-image button comes first");
        let picture = button
            .child()
            .and_downcast::<gtk::Picture>()
            .expect("the button wraps the picture");
        let badge = button
            .next_sibling()
            .and_downcast::<gtk::Label>()
            .expect("the c2pa-badge follows the button");
        (picture, badge)
    }

    /// Pump `ctx` until `done` holds — bounded by iterations, never by a clock.
    fn pump_until(ctx: &glib::MainContext, done: impl Fn() -> bool) -> bool {
        for _ in 0..10_000 {
            if done() {
                return true;
            }
            ctx.iteration(false);
        }
        done()
    }

    /// Run `body` on the GTK test thread under a private main context, so the
    /// loads a card spawns run only when the test pumps them.
    fn on_private_context(body: impl FnOnce(&glib::MainContext) + Send + 'static) {
        crate::testid::run_on_gtk_thread(move || {
            let ctx = glib::MainContext::new();
            ctx.with_thread_default(|| body(&ctx))
                .expect("acquire a private main context");
        });
    }

    /// A client whose nest is a closed port — nothing here dials a real nest
    /// (`walk.rs`'s own fixture idiom); the receiver is handed back so every
    /// `UiSender::send` has somewhere to land.
    fn offline_client() -> (
        Rc<FaunaClient>,
        std::sync::mpsc::Receiver<crate::app::UiMessage>,
    ) {
        let (tx, rx) = crate::client::ui_channel();
        let machine = fauna_launch_machine::LaunchMachine::new(
            Arc::new(fauna_launch_machine::NullObserver),
            Arc::new(fauna_launch_machine::InMemoryPersistence::new()),
        );
        let client = Rc::new(FaunaClient::new(
            "http://127.0.0.1:1".to_string(),
            "11".repeat(32),
            tx,
            machine,
        ));
        (client, rx)
    }

    /// **A restored tier-gated draft whose tier is not loaded yet never posts
    /// public.** After a restart the posts draft rail puts the draft's tier
    /// back into the manager, but the select can only show it once
    /// `own_tiers` lands — until then (and for as long as that refresh keeps
    /// failing) it shows Public. The Post click used to re-read the SELECT and
    /// re-stage its Public over the manager's tier, so the draft went out to
    /// everyone. It submits the manager's audience now, like the compose
    /// dialog, and the shared tier resolution fails closed on the unknown tier
    /// (`feed.compose_gate_no_key`) rather than publishing it.
    ///
    /// Reds when the inline submit reads the audience off the control again.
    #[test]
    fn a_restored_tier_draft_whose_tier_is_not_loaded_never_posts_public() {
        on_private_context(|ctx| {
            let manager = offline_manager();
            // What the posts draft rail restores on a restart.
            manager.update_compose_gate(Some("Gold".into()), "a teaser".into());
            let (client, _rx) = offline_client();
            let (compose_box, _, audience, text_view, _, refreshing, _, _) =
                build_compose_post_wired(&manager, &client);

            // `render_posts`'s paint, with `own_tiers` still empty.
            refreshing.set(true);
            audience.paint(&manager.snapshot().compose);
            refreshing.set(false);
            assert_eq!(
                audience.answer(),
                GateAnswer::Public,
                "the fixture must show the unloaded tier as Public, or the click below is vacuous"
            );

            text_view.buffer().set_text("members only");
            let root: gtk::Widget = compose_box.upcast();
            crate::automation::find::find_in(&root, ids::POST_SUBMIT_BUTTON)
                .and_downcast::<gtk::Button>()
                .expect("the composer carries its post-submit-button")
                .emit_clicked();

            // The submit runs on the client's runtime and reports back through
            // the GTK context, so each blocking iteration is a real event.
            let settled = || {
                let c = manager.snapshot().compose;
                c.error.is_some() || c.gate_tier.is_none()
            };
            for _ in 0..10_000 {
                if settled() {
                    break;
                }
                ctx.iteration(true);
            }
            let compose = manager.snapshot().compose;
            assert_eq!(
                compose.gate_tier.as_deref(),
                Some("Gold"),
                "the Post click must keep the restored tier, never re-stage the select's Public"
            );
            assert_eq!(
                compose.error.map(|e| e.key).as_deref(),
                Some("feed.compose_gate_no_key"),
                "an unresolvable tier fails the submit closed"
            );
        });
    }

    /// The repaint pin: a feed card rebuilt from the same snapshot — while its
    /// image is still loading, and again after it painted — issues no second
    /// blob GET and no second C2PA check, and the card rebuilt after the paint
    /// shows the image and the badge at once, from what the first load
    /// produced. Before the cache every build issued one of each (and decoded
    /// the bytes on the GTK thread again).
    #[test]
    fn a_repaint_of_a_painted_post_image_fetches_nothing() {
        on_private_context(|ctx| {
            let manager = offline_manager();
            let nest = CountingMedia::default();
            let hash = hex::encode([0xabu8; 32]);

            let _first = build_post_image_from(&hash, &manager, &nest);
            let rebuilt_while_loading = build_post_image_from(&hash, &manager, &nest);
            assert_eq!(
                nest.counts(),
                (1, 1),
                "a rebuild while the image loads must not start a second load"
            );

            nest.answer(0, &png(), true);
            let (picture, badge) = image_parts(&rebuilt_while_loading);
            assert!(
                pump_until(ctx, || picture.paintable().is_some() && badge.is_visible()),
                "the card rebuilt while loading must be painted when the one load lands"
            );

            let repainted = build_post_image_from(&hash, &manager, &nest);
            assert_eq!(
                nest.counts(),
                (1, 1),
                "a repaint of a painted image must issue no GET and no C2PA check"
            );
            let (picture, badge) = image_parts(&repainted);
            assert!(
                picture.paintable().is_some(),
                "the repainted card must show the cached texture without waiting"
            );
            assert!(
                badge.is_visible(),
                "the repainted card must show the cached verdict without waiting"
            );
        });
    }

    /// A bridged post's `ProxiedVideo` (render-model.md § D6c → *Proxied
    /// video*) paints `video-thumbnail` as the play glyph + its path — the slot
    /// the shared `proxied_post_video` picks — and the builder takes no
    /// client, so nothing is fetched for it.
    #[test]
    fn a_bridged_proxied_video_paints_its_path_in_video_thumbnail() {
        crate::testid::run_on_gtk_thread(|| {
            let path = "/api/v1/media/proxy?url=https%3A%2F%2Fcdn.bsky.app%2Fclip.mp4";
            let doc = fauna_core::render::RenderDocument {
                blocks: vec![fauna_core::render::RenderBlock::ProxiedVideo {
                    path: path.into(),
                    alt: String::new(),
                }],
            };
            let slot = doc
                .proxied_post_video()
                .expect("a bridged video takes the slot");
            let thumb = build_post_video(slot);
            assert!(
                thumb.first_child().and_downcast::<gtk::Image>().is_some(),
                "the play glyph comes first"
            );
            let label = thumb
                .last_child()
                .and_downcast::<gtk::Label>()
                .expect("the path label follows the glyph");
            assert_eq!(label.text(), path);
        });
    }

    /// A bridged post's `ProxiedImage` (render-model.md § D6c) paints in
    /// `post-image` from ONE GET of its nest-relative path — never a blob GET
    /// (`/api/v1/blob/000…`), never a C2PA check — its automation text is the
    /// path before and after the bytes land, and a repaint is served from the
    /// cache keyed by that path.
    #[test]
    fn a_bridged_proxied_image_paints_from_its_path_and_never_asks_for_a_blob() {
        on_private_context(|ctx| {
            let manager = offline_manager();
            let nest = CountingMedia::default();
            let path = "/api/v1/bluesky/media?url=https%3A%2F%2Fcdn.bsky.app%2Fimg%2Fa.jpg";

            let first = build_post_proxied_image_from(path, &manager, &nest);
            let button = first
                .first_child()
                .and_downcast::<gtk::Button>()
                .expect("the post-image button comes first");
            assert_eq!(crate::automation::find::text_of(button.upcast_ref()), path);
            assert!(
                button.next_sibling().is_none(),
                "a proxied image carries no c2pa-badge"
            );
            let _rebuilt = build_post_proxied_image_from(path, &manager, &nest);
            assert_eq!(nest.counts(), (0, 0), "no blob GET and no C2PA check");
            assert_eq!(
                nest.proxied
                    .borrow()
                    .iter()
                    .map(|(p, _)| p.as_str())
                    .collect::<Vec<_>>(),
                vec![path],
                "one GET of the proxied path, however often the card is rebuilt"
            );

            let _ = nest.proxied.borrow()[0].1.try_send(Ok(png()));
            let picture = button
                .child()
                .and_downcast::<gtk::Picture>()
                .expect("the button wraps the picture");
            assert!(
                pump_until(ctx, || picture.paintable().is_some()),
                "the proxied bytes paint as they come — no open step"
            );

            let repainted = build_post_proxied_image_from(path, &manager, &nest);
            assert_eq!(nest.proxied.borrow().len(), 1, "a repaint fetches nothing");
            let picture = repainted
                .first_child()
                .and_downcast::<gtk::Button>()
                .and_then(|b| b.child())
                .and_downcast::<gtk::Picture>()
                .expect("the repainted card's picture");
            assert!(
                picture.paintable().is_some(),
                "painted from the cache at once"
            );
        });
    }

    /// A nest that could not serve the image right now (a timeout, a `503`
    /// under load) does not blank it for the reader's session: the failure is
    /// forgotten, so the next rebuild fetches again and its success paints —
    /// where a refused GET (below) is settled for good.
    #[test]
    fn a_transient_post_image_failure_is_retried_by_the_next_rebuild() {
        on_private_context(|ctx| {
            let manager = offline_manager();
            let nest = CountingMedia::default();
            let hash = hex::encode([0x21u8; 32]);

            let _ = build_post_image_from(&hash, &manager, &nest);
            let _ = nest.gets.borrow()[0].try_send(Err(ApiError::Transport("timed out".into())));
            let _ = nest.checks.borrow()[0].try_send(Ok(false));
            while ctx.iteration(false) {}

            let retried = build_post_image_from(&hash, &manager, &nest);
            assert_eq!(
                nest.counts(),
                (2, 1),
                "the image is fetched again; the settled verdict is not re-asked"
            );
            let _ = nest.gets.borrow()[1].try_send(Ok(png()));
            let (picture, _) = image_parts(&retried);
            assert!(pump_until(ctx, || picture.paintable().is_some()));
        });
    }

    #[test]
    fn a_refused_post_image_is_never_refetched() {
        on_private_context(|ctx| {
            let manager = offline_manager();
            let nest = CountingMedia::default();
            let hash = hex::encode([0x22u8; 32]);

            let _ = build_post_image_from(&hash, &manager, &nest);
            let _ = nest.gets.borrow()[0].try_send(Err(ApiError::Status {
                code: 404,
                message: "no such blob".into(),
            }));
            let _ = nest.checks.borrow()[0].try_send(Ok(false));
            while ctx.iteration(false) {}

            let _ = build_post_image_from(&hash, &manager, &nest);
            assert_eq!(nest.counts(), (1, 1));
        });
    }

    /// The cache is one reader's: a new manager (every sign-in builds one)
    /// starts empty, so an image the previous reader loaded — possibly a gated
    /// post's photo opened under a key only they held — is fetched afresh and
    /// never painted from the old cache.
    #[test]
    fn a_new_reader_does_not_inherit_the_previous_readers_images() {
        on_private_context(|ctx| {
            let nest = CountingMedia::default();
            let hash = hex::encode([0xcdu8; 32]);

            let first_reader = offline_manager();
            let card = build_post_image_from(&hash, &first_reader, &nest);
            nest.answer(0, &png(), true);
            let (picture, _) = image_parts(&card);
            assert!(pump_until(ctx, || picture.paintable().is_some()));

            let next_reader = offline_manager();
            let card = build_post_image_from(&hash, &next_reader, &nest);
            assert_eq!(nest.counts(), (2, 2), "a new reader loads for itself");
            let (picture, badge) = image_parts(&card);
            assert!(picture.paintable().is_none());
            assert!(!badge.is_visible());
        });
    }

    /// A load that lands after its reader was replaced is discarded, never
    /// folded into the next reader's cache: that reader's own load is still the
    /// one in flight, and a card it builds waits for it.
    #[test]
    fn a_load_landing_after_its_reader_left_is_discarded() {
        on_private_context(|ctx| {
            let nest = CountingMedia::default();
            let hash = hex::encode([0xefu8; 32]);

            let first_reader = offline_manager();
            let _ = build_post_image_from(&hash, &first_reader, &nest);
            let next_reader = offline_manager();
            let _ = build_post_image_from(&hash, &next_reader, &nest);

            nest.answer(0, &png(), true);
            while ctx.iteration(false) {}

            let card = build_post_image_from(&hash, &next_reader, &nest);
            assert_eq!(
                nest.counts(),
                (2, 2),
                "the next reader's load is still in flight"
            );
            let (picture, badge) = image_parts(&card);
            assert!(
                picture.paintable().is_none(),
                "the departed reader's image must not paint for the next one"
            );
            assert!(!badge.is_visible());
        });
    }

    /// The actor-change teardown drops the cache outright, even for a manager
    /// that were somehow reused.
    #[test]
    fn the_identity_change_teardown_empties_the_cache() {
        on_private_context(|ctx| {
            let manager = offline_manager();
            let nest = CountingMedia::default();
            let hash = hex::encode([0x12u8; 32]);

            let card = build_post_image_from(&hash, &manager, &nest);
            nest.answer(0, &png(), false);
            let (picture, _) = image_parts(&card);
            assert!(pump_until(ctx, || picture.paintable().is_some()));

            crate::media_loads::clear_for_identity_change();
            let _ = build_post_image_from(&hash, &manager, &nest);
            assert_eq!(nest.counts(), (2, 2));
        });
    }
}
