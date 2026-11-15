//! The Profile page — the canonical per-user *detail* surface (`profile.md`).
//! It renders **either** the viewer's own profile (SELF, reached via the
//! top-level `profile-tab`) **or** another actor's (reached by tap-through —
//! e.g. a contact row), branching on `is_self` (the `target` argument). Both
//! show an identity header (`user-header`) + a tab strip (`profile-posts-tab`
//! · `profile-tiers-tab`). The Posts tab lists the viewed actor's posts as the
//! feed paints them, over an author window of the feed manager (`posts`).
//! The Tiers tab hosts the subscriptions
//! Slice-A author management (`tiers`) on the SELF profile, or the Slice-B
//! subscriber-browse offers section (`offers`) on another's.
//!
//! The header renders the actor's **display_name** when they have published a
//! `Profile` (`fauna.profile.get` → `fauna_client_profile::decode_profile_display`),
//! falling back to the **handle** (account cache, SELF only) then the actor_id.
//! On another's profile the name goes through the one shared resolver over the
//! viewer's own nickname for them (`value-formatting.md` § Peer display label):
//! a nickname heads the page and the public name it replaced stays beneath it
//! (`profile-public-name`), and the **private section** ([`private`]) edits
//! that nickname, the notes and the labels.
//! The stored avatar and banner (`profile-avatar` / `profile-banner`) paint
//! once fetched from the nest that served the profile ([`HeaderPictures`]);
//! the generic avatar icon stands in while the profile names none. The primary
//! relationship action is `profile-edit-button` (own profile → the text-only
//! edit form `edit`, publishing via `fauna.profile.set`) or
//! `profile-follow-button` (another's → subscribe to the free "followers" tier).

pub mod edit;
pub mod offers;
mod posts;
pub mod private;
pub mod tiers;

use fauna_ui_ids as ids;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;

use fauna_client_profile::{ProfileClient, decode_profile_display};
use fauna_client_subscriptions::SubscriptionsClient;
use fauna_conversations::TypedAddress;
use fauna_core::identity::ActorId;

use self::offers::FOLLOWERS_TIER;
use crate::async_helper::spawn_with_snapshot;
use crate::client::{FaunaClient, KnockSend, load_account_cache};
use crate::conversations::overlays;
use crate::i18n::strings::contacts as contacts_strings;
use crate::i18n::strings::profile as p;
use crate::testid::set_test_id;
use crate::views::contacts::guardian_ask::GuardianAsk;

/// Open a Profile page: `(target, home)` — `target` is `None` for the viewer's
/// own profile (SELF) or `Some(actor hex)` for another's; `home` is the routing
/// hint the open carries (where the viewer met the actor — never identity). The
/// one opener every tap-through and the state-protocol nav patch resolve to.
pub type OpenProfile = Rc<dyn Fn(Option<String>, Option<String>)>;

thread_local! {
    /// The live main window's opener, registered by `app.rs` when it builds the
    /// window ([`register_opener`]). The doors that live deep in other pages —
    /// the Find User result, a post's author line — reach the Profile page
    /// through it instead of each threading a callback down their builders.
    static OPENER: RefCell<Option<OpenProfile>> = const { RefCell::new(None) };
}

/// Point the doors at the main window's Profile opener (the window rebuilt on
/// every sign-in registers its own).
pub fn register_opener(open: OpenProfile) {
    OPENER.with(|slot| *slot.borrow_mut() = Some(open));
}

/// Open another actor's Profile with the home the viewer met them on carried
/// (`profile.md` § Layout & flow → *Reaching another actor's profile with their
/// home carried*): an id naming the viewer opens SELF through the shared
/// `fauna_core::format::profile_nav_target`, never their own profile in OTHER
/// shape. Answers `false` when no main window has registered an opener.
pub fn open_actor(client: &FaunaClient, actor_id: &str, home: Option<String>) -> bool {
    let Some(open) = OPENER.with(|slot| slot.borrow().clone()) else {
        return false;
    };
    let me = client.actor_id();
    open(
        fauna_core::format::profile_nav_target(actor_id, me.as_deref()),
        home,
    );
    true
}

/// The page's actor-home slot: the OTHER actor's home nest URL (`None` = this
/// nest) that the follow, the knock and the remote header read all read
/// (`profile.md` § Where logic lives → *The actor's home*).
type HomeSlot = Rc<RefCell<Option<String>>>;

/// The home a write to the viewed actor carries, read off the page's slot at
/// press time — the one read the follow and the Tiers tab's claim redeem share,
/// so a code redeems exactly where the follow would subscribe.
fn press_home(slot: &RefCell<Option<String>>) -> Option<String> {
    slot.borrow().clone()
}

/// What the open page reports as `data.profile` (ui.yaml `profile.is_self`,
/// `profile.actor_id`, `profile.home_nest_url`).
struct PageReport {
    is_self: bool,
    actor_id: String,
    actor_home: HomeSlot,
}

thread_local! {
    /// The most recently built Profile page — the page is rebuilt per target, so
    /// the newest build is the one on screen (or about to be).
    static PAGE_REPORT: RefCell<Option<PageReport>> = const { RefCell::new(None) };
}

fn report_page(is_self: bool, actor_id: &str, actor_home: &HomeSlot) {
    PAGE_REPORT.with(|report| {
        *report.borrow_mut() = Some(PageReport {
            is_self,
            actor_id: actor_id.to_string(),
            actor_home: Rc::clone(actor_home),
        });
    });
}

/// The state-protocol read of the Profile page: `{is_self, actor_id,
/// home_nest_url}` (`home_nest_url` is the actor-home slot, `null` = this
/// nest), `null` before any page was built.
pub fn state_json() -> serde_json::Value {
    PAGE_REPORT.with(|report| match report.borrow().as_ref() {
        Some(r) => serde_json::json!({
            "is_self": r.is_self,
            "actor_id": r.actor_id,
            "home_nest_url": r.actor_home.borrow().clone(),
        }),
        None => serde_json::Value::Null,
    })
}

/// Handles the app retains. The page refreshes itself on each mutation
/// (observer-free, `feed.md` § Architectural rules); `refresh_tiers` additionally
/// lets `app.rs` re-read the Tiers-tab sections when the profile page becomes
/// visible (mirroring the Peers page's on-visible refresh), so a pending
/// subscribe request that arrived while the author was elsewhere shows up.
pub struct ProfileHandles {
    pub refresh_tiers: Rc<dyn Fn()>,
    /// Open the Posts tab afresh — a new author window and its first page
    /// (`profile.md` § The Posts tab: every open reads, no client cache).
    /// `app.rs` runs it when the page becomes visible, beside `refresh_tiers`.
    pub open_posts: Rc<dyn Fn()>,
    /// Re-read the header's published name and pictures (`profile.md`
    /// § Persistence: no client cache, fetched on demand). `app.rs` runs it
    /// when the page becomes visible: the SELF page is built once, and a
    /// profile written elsewhere (the archive import's Done-step prefill)
    /// must show on the next visit.
    pub refresh_header: Rc<dyn Fn()>,
}

/// Build the Profile page. `target` is the viewed actor: `None` = the viewer's
/// own profile (SELF — reached via the `profile-tab` sidebar row); `Some(hex)` =
/// another actor's profile, reached by tap-through (`profile.md` § Layout &
/// flow). Everything below branches on `is_self`: the header shows the
/// `profile-edit-button` (self) or `profile-follow-button` (other), and the
/// Tiers tab hosts the SELF author-management sections (`tiers`) or the OTHER
/// subscriber-browse offers section (`offers`). The page is rebuilt per target
/// by `app.rs`'s `open_profile`. `carried_home` is the open target's routing
/// hint — where the viewer met the actor (`profile.md` § Layout & flow →
/// *Reaching another actor's profile with their home carried*); it only seeds
/// the page's actor-home slot ([`HomeSlot`]). `open_post` opens the feed page's
/// `post_detail` for a post id — the door a Posts-tab card activation uses.
pub fn build_profile_view(
    client: &Rc<FaunaClient>,
    target: Option<String>,
    carried_home: Option<String>,
    on_open_conversations: Rc<dyn Fn()>,
    open_post: Rc<dyn Fn(String)>,
) -> (gtk::Box, ProfileHandles) {
    let is_self = target.is_none();
    let actor_id = target.unwrap_or_else(|| client.actor_id().unwrap_or_default());

    let outer = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .build();
    set_test_id(&outer, ids::PROFILE_VIEW);

    // OTHER: the one actor-home slot the follow, the knock and the remote
    // header read all read (`profile.md` § Where logic lives → *The actor's
    // home*), seeded at once from the carried hint through the shared rule so a
    // follow pressed before the header read answers already routes. SELF has
    // no other actor's home, so a hint there is dropped.
    let carried_home = carried_home.filter(|_| !is_self);
    let actor_home: HomeSlot = Rc::new(RefCell::new(actor_home_for(
        carried_home.as_deref(),
        &actor_id,
        None,
        client.node_url(),
    )));
    report_page(is_self, &actor_id, &actor_home);

    let (header, header_widgets) = build_header(client, &actor_id, is_self, &actor_home);
    outer.append(&header_widgets.pictures.banner);
    outer.append(&header);

    // Page-level error-message (Rule 2), shared into the Tiers tab so its
    // mint/roster failures surface here, and written by the OTHER profile's
    // knock / guardian ask. Built here, appended below the tab strip.
    let error_label = gtk::Label::builder().visible(false).build();
    error_label.add_css_class("error");
    error_label.set_halign(gtk::Align::Start);
    error_label.set_wrap(true);
    error_label.set_margin_start(12);
    set_test_id(&error_label, ids::ERROR_MESSAGE);

    // Secondary relationship actions — OTHER profile only (`profile.md` § Layout
    // & flow: "on another's profile only, secondary relationship actions"):
    // Start DM, Block ⇄ Unblock, and Request contact (the knock) with the
    // supervised ward's guardian-ask pair beside it.
    if !is_self {
        let actions = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .margin_start(12)
            .margin_end(12)
            .build();

        // Start DM — pure nav glue: seed the Conversations new-thread composer
        // with this actor and switch to the Conversations page (no new kind, no
        // persistence; `profile.md` § Where logic lives → Start DM action).
        let dm_btn = gtk::Button::with_label(p::START_DM);
        set_test_id(&dm_btn, ids::PROFILE_START_DM_BUTTON);
        {
            let actor_id = actor_id.clone();
            let on_open_conversations = Rc::clone(&on_open_conversations);
            dm_btn.connect_clicked(move |_| start_dm(&actor_id, &on_open_conversations));
        }
        actions.append(&dm_btn);

        // Block ⇄ Unblock — the one toggle whose label flips on the viewed actor's
        // `contact_status` (`profile.md` § User actions): a `blocked` edge reads
        // "Unblock", any other/absent edge reads "Block". Block calls the shared
        // `fauna_client_contacts::knocks_block` over `fauna.knocks.block` (upsert →
        // `blocked`); unblock calls `knocks_unblock` over `fauna.knocks.unblock`
        // (the guarded clear-the-edge, `ContactStatus` → `None`; `contacts.md` §
        // Where logic lives → Unblock). The initial state is read from
        // `fauna.contacts.list` (`refresh_block_state`); each tap toggles and
        // re-renders from the new state. `block_user_authored` guards the
        // open-time read against a press that beat it home — see `apply_block_read`.
        let block_btn = new_block_button();
        let is_blocked = Rc::new(Cell::new(false));
        let block_user_authored = Rc::new(Cell::new(false));
        refresh_block_state(
            client,
            &actor_id,
            &block_btn,
            &is_blocked,
            &block_user_authored,
        );
        {
            let client = Rc::clone(client);
            let actor_id = actor_id.clone();
            let is_blocked = Rc::clone(&is_blocked);
            let block_user_authored = Rc::clone(&block_user_authored);
            block_btn.connect_clicked(move |btn| {
                toggle_block(&client, &actor_id, btn, &is_blocked, &block_user_authored)
            });
        }
        actions.append(&block_btn);

        // Report — tell the nest's admins about this account (`moderation.md`
        // § User-initiated reporting → App surface), beside Block. The sheet's
        // *also block this person* runs the same `knocks_block` the button
        // above does, so a landed one re-renders THIS page's Block ⇄ Unblock
        // the way a tap would.
        let report_btn = gtk::Button::with_label(p::REPORT);
        set_test_id(&report_btn, ids::PROFILE_REPORT_BUTTON);
        {
            let client = Rc::clone(client);
            let actor_id = actor_id.clone();
            let block_btn = block_btn.clone();
            let is_blocked = Rc::clone(&is_blocked);
            let block_user_authored = Rc::clone(&block_user_authored);
            report_btn.connect_clicked(move |b| {
                let block_btn = block_btn.clone();
                let is_blocked = Rc::clone(&is_blocked);
                let block_user_authored = Rc::clone(&block_user_authored);
                let on_blocked: Rc<dyn Fn(&str)> = Rc::new(move |_| {
                    is_blocked.set(true);
                    block_user_authored.set(true);
                    apply_block_state(&block_btn, true);
                    // The sheet's block authors the edge: the toggle is known.
                    block_btn.set_sensitive(true);
                });
                crate::report::open_sheet(
                    b,
                    crate::report::actor_target(&actor_id),
                    &client,
                    Some(on_blocked),
                );
            });
        }
        actions.append(&report_btn);

        // Request contact — the knock, routed to the viewed actor's home nest,
        // with the ward's ask pair (`family-safety.md` § Child-initiated contact
        // requests → *App affordance*) offered only on the TYPED refusal.
        let (request_btn, ask) = build_request_contact(&actor_id);
        wire_request_contact(
            client,
            &actor_id,
            &request_btn,
            &ask,
            &error_label,
            &actor_home,
        );
        actions.append(&request_btn);
        actions.append(ask.widget());

        outer.append(&actions);

        // The private section — the viewer's own nickname, notes and labels on
        // this person, below the relationship actions and above the tab strip
        // (`profile.md` § The private section). The overlay projection moving
        // (this page's Save, a sibling device's edit) re-paints the header's
        // names and the section's untouched fields; the watch is armed before
        // either paints.
        let repaint_section = Rc::new(RefCell::new(None::<Rc<dyn Fn()>>));
        {
            let names = header_widgets.names.clone();
            let actor_id = actor_id.clone();
            let repaint_section = Rc::clone(&repaint_section);
            overlays::watch(&outer, move || {
                names.paint(&actor_id);
                if let Some(repaint) = repaint_section.borrow().as_ref() {
                    repaint();
                }
            });
        }
        let (section, repaint) = private::build(client, &actor_id, error_label.clone());
        *repaint_section.borrow_mut() = Some(repaint);
        outer.append(&section);
    }

    // page-heading mirrors the active tab (ui.yaml `heading: S.profile.posts`).
    let heading = gtk::Label::new(Some(p::POSTS));
    heading.set_halign(gtk::Align::Start);
    heading.add_css_class("title-2");
    heading.set_margin_start(12);
    set_test_id(&heading, ids::PAGE_HEADING);
    outer.append(&heading);

    // Tab strip — two buttons driving the inner stack.
    let tabs = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .margin_start(12)
        .build();
    let posts_tab = gtk::Button::with_label(p::POSTS);
    set_test_id(&posts_tab, ids::PROFILE_POSTS_TAB);
    let tiers_tab = gtk::Button::with_label(p::TIERS);
    set_test_id(&tiers_tab, ids::PROFILE_TIERS_TAB);
    tabs.append(&posts_tab);
    tabs.append(&tiers_tab);
    outer.append(&tabs);

    outer.append(&error_label);

    // Edit form (text-only; hidden until `profile-edit-button`). Save publishes
    // via `fauna.profile.set` and calls `refresh_header` to re-render the header
    // identity (the published display_name).
    // The published display_name (once `fauna.profile.get` returns) feeds the
    // header's names: on the SELF profile over the cached handle, then the
    // actor_id; on another's through the shared resolver ([`HeaderNames`]).
    // Filled once the OTHER Tiers tab is built (below): the header read calls
    // it after writing the slot.
    let home_moved: HomeMovedHook = Rc::new(RefCell::new(None));
    let refresh_header: Rc<dyn Fn()> = {
        let client = Rc::clone(client);
        let names = header_widgets.names.clone();
        let pictures = header_widgets.pictures.clone();
        let actor_id = actor_id.clone();
        // SELF has no home to resolve, so no slot to fill.
        let home = (!is_self).then(|| HeaderHome {
            slot: Rc::clone(&actor_home),
            carried: carried_home.clone(),
            moved: Rc::clone(&home_moved),
        });
        Rc::new(move || refresh_header_name(&client, &names, &pictures, &actor_id, home.clone()))
    };
    // The text-only edit form (publish via `fauna.profile.set`) is SELF-only.
    if let Some(edit_btn) = &header_widgets.edit_btn {
        let (edit_form, edit_handle) =
            edit::build_edit_form(client, error_label.clone(), refresh_header.clone());
        outer.append(&edit_form);
        let open = Rc::clone(&edit_handle.open);
        edit_btn.connect_clicked(move |_| open());
    }
    // Render the published display_name into the header (async; falls back to
    // the handle/actor_id on not_found).
    refresh_header();

    // Inner stack: posts (the viewed actor's posts as the feed paints them —
    // `posts`) / tiers (SELF author-management sections, or — for another
    // actor — the subscriber-browse offers section).
    let stack = gtk::Stack::new();
    stack.set_vexpand(true);

    let (posts_page, open_posts) = posts::build(client, &actor_id, error_label.clone(), open_post);
    stack.add_named(&posts_page, Some("posts"));

    let (tiers_page, refresh_tiers) = if is_self {
        tiers::build_tiers_tab(client, error_label.clone())
    } else {
        // Another actor's offered tiers (subscriber browse), read from the
        // page's actor-home slot. A malformed target hex degrades to an empty
        // offers list rather than panicking.
        let author = ActorId::from_hex(&actor_id).unwrap_or(ActorId([0u8; 32]));
        let (page, refresh, moved) =
            offers::build_offers_tab(client, error_label.clone(), author, Rc::clone(&actor_home));
        *home_moved.borrow_mut() = Some(moved);
        (page, refresh)
    };
    stack.add_named(&tiers_page, Some("tiers"));
    stack.set_visible_child_name("posts");
    outer.append(&stack);

    {
        let stack = stack.clone();
        let heading = heading.clone();
        // Every activation reads the author's posts afresh (`profile.md` § The
        // Posts tab → *Order and paging*), as tui's `profile-posts-tab` does.
        let open = Rc::clone(&open_posts);
        posts_tab.connect_clicked(move |_| {
            stack.set_visible_child_name("posts");
            heading.set_text(p::POSTS);
            open();
        });
    }
    {
        let stack = stack.clone();
        let heading = heading.clone();
        // Every activation of the tab re-reads that tab's data — the ruled
        // uniform door (`monetization.md` § Pillar 1 → *The Tiers-tab re-read
        // door*), lifting tui's `Action::ShowTiers` shape. Unconditional on
        // purpose: a re-click while the tab is already showing is exactly how a
        // subscriber asks "did the author approve me yet?", and there is no push
        // kind for a subscribe grant to answer it otherwise. Before this the
        // click only swapped the inner stack, so on linux the answer was "never".
        let refresh = Rc::clone(&refresh_tiers);
        tiers_tab.connect_clicked(move |_| {
            stack.set_visible_child_name("tiers");
            heading.set_text(p::TIERS);
            refresh();
        });
    }

    (
        outer,
        ProfileHandles {
            refresh_tiers,
            open_posts,
            refresh_header,
        },
    )
}

/// Header widgets the page wires after construction: the identity `names`
/// (refreshed with the published display_name) and — SELF only — the `edit_btn`
/// (opens the edit form). On another actor's profile the `profile-follow-button`
/// is built and wired here (no `edit_btn`), so `edit_btn` is `None`.
struct HeaderWidgets {
    names: HeaderNames,
    pictures: HeaderPictures,
    edit_btn: Option<gtk::Button>,
}

/// The header's identity lines: `profile-handle`, and `profile-public-name`
/// beneath it — rendered only while the viewer's nickname is the primary line,
/// carrying the public name the nickname replaced (`profile.md` § The private
/// section → *The header shows both names*).
#[derive(Clone)]
struct HeaderNames {
    primary: gtk::Label,
    public: gtk::Label,
    /// The published display name, once the open's `fauna.profile.get` lands.
    display_name: Rc<RefCell<Option<String>>>,
    /// The SELF profile, whose fallback is [`self_header_fallback`] read at
    /// every paint; another's names come from the shared resolver.
    is_self: bool,
}

thread_local! {
    /// Repainters for every SELF header still on screen — each answers `false`
    /// once its labels are gone, and is dropped then. Driven by
    /// [`repaint_self_headers`] on every identity refresh.
    static SELF_HEADERS: RefCell<Vec<Box<dyn Fn() -> bool>>> = const { RefCell::new(Vec::new()) };
}

/// Repaint every live SELF profile header — called on each
/// `IdentityRefreshed`, so the header follows the account's current primary
/// (`mail-primary-domain-rename.md` § Display-handle interaction) without a
/// page rebuild. tui's twin is the `SelfAddressRefreshed` push.
pub fn repaint_self_headers() {
    SELF_HEADERS.with(|headers| headers.borrow_mut().retain(|repaint| repaint()));
}

/// The SELF header's fallback when no display name is published: the handle on
/// the CURRENT primary, `<handle>@<domain>`, through the shared
/// `fauna_client_accounts::display_handle` — never the bare handle — else the
/// actor id. Read at paint time from the live account cache.
fn self_header_fallback(actor_id: &str) -> String {
    let (handle, domain, _tier) = load_account_cache();
    let handle = handle.unwrap_or_default();
    fauna_client_accounts::display_handle(&handle, Some(&handle), domain.as_deref())
        .unwrap_or_else(|| actor_id.to_string())
}

impl HeaderNames {
    /// The two lines for `actor_id`: SELF = published display name → the
    /// fallback; OTHER = the one shared resolver over the viewer's nickname,
    /// the published display name, then the canonical short id (the OTHER
    /// header holds no handle).
    fn label(&self, actor_id: &str) -> fauna_core::format::PeerLabel {
        let display_name = self.display_name.borrow();
        if self.is_self {
            fauna_core::format::PeerLabel {
                primary: display_name
                    .clone()
                    .unwrap_or_else(|| self_header_fallback(actor_id)),
                public: None,
            }
        } else {
            overlays::projection().peer_label(display_name.as_deref(), None, actor_id)
        }
    }

    fn paint(&self, actor_id: &str) {
        let label = self.label(actor_id);
        self.primary.set_text(&label.primary);
        self.public
            .set_text(label.public.as_deref().unwrap_or_default());
        self.public.set_visible(label.public.is_some());
    }
}

/// The shared `user-header`: avatar placeholder + display_name/handle +
/// actor-id copy + the primary relationship action (`profile-edit-button` on
/// own profile, `profile-follow-button` on another's — follow = subscribe to the
/// free "followers" tier; `profile.md` § Layout & flow). The name renders the
/// handle/actor_id synchronously; `refresh_header_name` then overwrites it with
/// the published display_name (if any).
fn build_header(
    client: &Rc<FaunaClient>,
    actor_id: &str,
    is_self: bool,
    actor_home: &HomeSlot,
) -> (gtk::Box, HeaderWidgets) {
    let header = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .margin_top(12)
        .margin_start(12)
        .margin_end(12)
        .build();

    let pictures = HeaderPictures::new();
    header.append(&pictures.avatar_icon);
    header.append(&pictures.avatar);

    let info = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .build();
    // Initial names: own `<handle>@<current primary>` on the SELF profile
    // ([`self_header_fallback`], re-read on every identity refresh), else the
    // resolver's answer with no published name yet. `refresh_header_name`
    // upgrades this to the published display_name once `fauna.profile.get`
    // returns.
    let handle_label = gtk::Label::new(None);
    handle_label.set_halign(gtk::Align::Start);
    handle_label.add_css_class("title-3");
    set_test_id(&handle_label, ids::PROFILE_HANDLE);
    info.append(&handle_label);
    let public_label = gtk::Label::new(None);
    public_label.set_halign(gtk::Align::Start);
    public_label.add_css_class("dim-label");
    public_label.set_visible(false);
    set_test_id(&public_label, ids::PROFILE_PUBLIC_NAME);
    info.append(&public_label);
    header.append(&info);
    let names = HeaderNames {
        primary: handle_label,
        public: public_label,
        display_name: Rc::new(RefCell::new(None)),
        is_self,
    };
    names.paint(actor_id);
    if is_self {
        let primary = names.primary.downgrade();
        let public = names.public.downgrade();
        let display_name = Rc::clone(&names.display_name);
        let actor_id = actor_id.to_string();
        SELF_HEADERS.with(|headers| {
            headers.borrow_mut().push(Box::new(move || {
                let (Some(primary), Some(public)) = (primary.upgrade(), public.upgrade()) else {
                    return false;
                };
                HeaderNames {
                    primary,
                    public,
                    display_name: Rc::clone(&display_name),
                    is_self: true,
                }
                .paint(&actor_id);
                true
            }));
        });
    }

    let copy_btn = gtk::Button::with_label(p::COPY_ID);
    copy_btn.set_valign(gtk::Align::Center);
    set_test_id(&copy_btn, ids::PROFILE_ACTOR_ID_COPY_BTN);
    // `copied` reports the exact string that reached the clipboard — the
    // settings copy-link buttons' contract. The header is rebuilt on every
    // profile open, so a fresh page's button carries no stale value.
    {
        let actor_id = actor_id.to_string();
        copy_btn.connect_clicked(move |btn| {
            crate::clipboard::copy_text(&actor_id);
            crate::testid::set_test_attr(btn, "copied", &actor_id);
        });
    }
    header.append(&copy_btn);

    let edit_btn = if is_self {
        let edit_btn = gtk::Button::with_label(p::EDIT);
        edit_btn.set_valign(gtk::Align::Center);
        set_test_id(&edit_btn, ids::PROFILE_EDIT_BUTTON);
        header.append(&edit_btn);
        Some(edit_btn)
    } else {
        // `profile-follow-button` is the Follow ⇄ Following toggle
        // (`subscriptions.md` § The free followers tier — follow and unfollow,
        // rule 4). It reads *Follow* until the open-time status read answers;
        // every decision (following, confirm-gated, the label) is shared Rust
        // (`fauna_core::format`), the state is [`FollowToggle`].
        let follow_btn = gtk::Button::with_label(
            &fauna_core::format::follow_toggle_label(false).resolve(crate::i18n::strings::lookup),
        );
        follow_btn.set_valign(gtk::Align::Center);
        set_test_id(&follow_btn, ids::PROFILE_FOLLOW_BUTTON);
        crate::offline_gate::declare_wire_kind(&follow_btn, "fauna.subscriptions.subscribe");
        let toggle = Rc::new(FollowToggle::new(&follow_btn));
        if let Ok(author) = ActorId::from_hex(actor_id) {
            toggle.read_status(client, author);
        }
        {
            let client = Rc::clone(client);
            let actor_id = actor_id.to_string();
            let actor_home = Rc::clone(actor_home);
            let toggle = Rc::clone(&toggle);
            follow_btn.connect_clicked(move |_| {
                let home = press_home(&actor_home);
                toggle.press(&client, &actor_id, home)
            });
        }
        header.append(&follow_btn);
        None
    };

    (
        header,
        HeaderWidgets {
            names,
            pictures,
            edit_btn,
        },
    )
}

/// The header's stored pictures (`profile-avatar`, `profile-banner`;
/// `profile.md` § Layout & flow, the `user-header` component): each element
/// renders only while the profile names its picture, and is empty — no
/// paintable, so `get_attr(<id>, state)` answers `placeholder` — until its
/// bytes paint, staying so after a failed fetch (never `error-message`). The
/// generic avatar icon stands in while the profile names no avatar.
#[derive(Clone)]
struct HeaderPictures {
    avatar_icon: gtk::Image,
    avatar: gtk::Picture,
    banner: gtk::Picture,
    /// Bumped by every header read, so a fetch an older read started (a save
    /// refetches both) never paints over a newer profile's pictures.
    epoch: Rc<Cell<u64>>,
}

impl HeaderPictures {
    fn new() -> Self {
        let avatar_icon = gtk::Image::from_icon_name("avatar-default-symbolic");
        avatar_icon.set_pixel_size(48);
        let avatar = gtk::Picture::new();
        avatar.set_size_request(48, 48);
        avatar.set_content_fit(gtk::ContentFit::Cover);
        avatar.set_valign(gtk::Align::Center);
        avatar.set_visible(false);
        set_test_id(&avatar, ids::PROFILE_AVATAR);
        let banner = gtk::Picture::new();
        banner.set_size_request(-1, 120);
        banner.set_content_fit(gtk::ContentFit::Cover);
        banner.set_visible(false);
        set_test_id(&banner, ids::PROFILE_BANNER);
        Self {
            avatar_icon,
            avatar,
            banner,
            epoch: Rc::new(Cell::new(0)),
        }
    }

    /// A header read answered: render exactly the elements the profile names,
    /// each empty until [`Self::paint`]. Returns the read's epoch.
    fn name(&self, avatar: bool, banner: bool) -> u64 {
        let epoch = self.epoch.get() + 1;
        self.epoch.set(epoch);
        for (picture, named) in [(&self.avatar, avatar), (&self.banner, banner)] {
            picture.set_paintable(None::<&gtk::gdk::Paintable>);
            picture.set_visible(named);
        }
        self.avatar_icon.set_visible(!avatar);
        epoch
    }

    /// Paint fetched `bytes` into `picture` — unless a newer read has named
    /// the pictures since, or the bytes will not decode (the element then
    /// stays empty).
    fn paint(&self, epoch: u64, picture: &gtk::Picture, bytes: &[u8]) {
        if epoch != self.epoch.get() {
            return;
        }
        if let Ok(texture) =
            gtk::gdk::Texture::from_bytes(&gtk::glib::Bytes::from_owned(bytes.to_vec()))
        {
            picture.set_paintable(Some(&texture));
        }
    }
}

/// The nest whose blob route serves the header's pictures: this one (`None`)
/// for a profile it served, the home a relayed profile came from otherwise
/// (`profile.md` § Where logic lives → *Bytes direct, as for a relayed post*),
/// pinned to the identity this nest stamped on the relay reply so a home
/// serving the self-signed floor paints; a reply with no stamp keeps the
/// WebPKI floor (`transport-trust.md` § The post arm → *A relayed profile's
/// pictures*).
fn picture_home(
    relayed_from: Option<&str>,
    home_nest_actor_id: Option<&str>,
) -> Option<fauna_client::ForeignHome> {
    relayed_from.map(|url| fauna_client::ForeignHome::new(url, home_nest_actor_id))
}

/// The two fields of the viewer's `fauna.subscriptions.status.get` reply the
/// follow toggle reads (tui's `FollowStatus`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct FollowStatus {
    tier: Option<String>,
    follow_pending: bool,
}

/// What a press on `profile-follow-button` does, from the toggle's state
/// (`subscriptions.md` § The free followers tier — follow and unfollow, rule 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FollowPress {
    /// Not following: subscribe to the free `followers` tier.
    Follow,
    /// A paid holder's first press on *Following*: relabel with the cost,
    /// send nothing.
    Arm,
    /// *Following* pressed by a free follower, or the confirming press.
    Unfollow,
}

/// The Follow ⇄ Following toggle's state, mirroring tui's `ProfileState`
/// fields (`status`, `follow_flip`, `unfollow_armed`): the open-time status
/// read, an optimistic flip set by a press's reply that outranks the status
/// for the label (a queued leave reads *Follow* at once), and the armed
/// confirm. The header is rebuilt on every profile open, so a fresh open
/// starts disarmed with no flip.
#[derive(Debug, Default)]
struct FollowState {
    status: Option<FollowStatus>,
    flip: Option<bool>,
    armed: bool,
}

impl FollowState {
    fn status_tier(&self) -> Option<&str> {
        self.status.as_ref().and_then(|s| s.tier.as_deref())
    }

    /// The flip outranks the status read (`format::is_following`); *Follow*
    /// until either says otherwise.
    fn is_following(&self) -> bool {
        self.flip.unwrap_or_else(|| {
            self.status.as_ref().is_some_and(|s| {
                fauna_core::format::is_following(s.tier.as_deref(), s.follow_pending)
            })
        })
    }

    /// Decide a press, arming or disarming the confirm as it goes.
    fn press(&mut self) -> FollowPress {
        if !self.is_following() {
            self.armed = false;
            return FollowPress::Follow;
        }
        if fauna_core::format::unfollow_ends_paid_tier(self.status_tier()) && !self.armed {
            self.armed = true;
            return FollowPress::Arm;
        }
        self.armed = false;
        FollowPress::Unfollow
    }

    /// The button's label: the cost while armed, else the shared map.
    fn label(&self) -> String {
        if self.armed {
            p::UNFOLLOW_CONFIRM.to_string()
        } else {
            fauna_core::format::follow_toggle_label(self.is_following())
                .resolve(crate::i18n::strings::lookup)
        }
    }

    /// A press's reply: the flip it authored, plus the status read after it
    /// (kept as it was when that read failed).
    fn replied(&mut self, followed: bool, status: Option<FollowStatus>) {
        self.flip = Some(followed);
        self.armed = false;
        if status.is_some() {
            self.status = status;
        }
    }
}

/// `status.get(author)` folded to [`FollowStatus`]; `None` when the read fails,
/// so a failure never reads as "not following".
async fn fetch_follow_status(
    subs: &SubscriptionsClient<std::sync::Arc<fauna_client::NestClient>>,
    author: ActorId,
) -> Option<FollowStatus> {
    let reply = subs.status_get(author).await.ok()?;
    Some(FollowStatus {
        tier: reply.tier,
        follow_pending: reply.follow_pending,
    })
}

/// The OTHER-profile follow toggle: its button and state.
struct FollowToggle {
    btn: gtk::glib::WeakRef<gtk::Button>,
    state: RefCell<FollowState>,
}

impl FollowToggle {
    fn new(btn: &gtk::Button) -> Self {
        Self {
            btn: btn.downgrade(),
            state: RefCell::new(FollowState::default()),
        }
    }

    fn repaint(&self) {
        if let Some(btn) = self.btn.upgrade() {
            btn.set_label(&self.state.borrow().label());
        }
    }

    /// The open-time status read; a press's reply that came first wins (its
    /// flip outranks the status for the label anyway).
    fn read_status(self: &Rc<Self>, client: &Rc<FaunaClient>, author: ActorId) {
        let nest = client.nest_rpc().clone();
        let this = Rc::clone(self);
        spawn_with_snapshot(
            &client.runtime_handle(),
            move || async move { fetch_follow_status(&SubscriptionsClient::new(nest), author).await },
            move |status| {
                if status.is_some() {
                    let mut st = this.state.borrow_mut();
                    if st.flip.is_none() {
                        st.status = status;
                    }
                }
                this.repaint();
            },
        );
    }

    /// A press on the toggle (`profile.md` § Where logic lives → *Follow /
    /// unfollow*): *Follow* subscribes to the free `followers` tier, *Following*
    /// leaves it — at once for a free follower, behind a second press when a
    /// paid tier is held, which the leave ends too. `home` is the page's
    /// actor-home slot as it reads at the press: the shared
    /// `SubscriptionsClient::{subscribe,unsubscribe}_across_nests` mints the
    /// signed intent when another nest is the author's home, and is the plain
    /// local call for `None` (`federated-follow.md` § Decision 2).
    fn press(self: &Rc<Self>, client: &Rc<FaunaClient>, actor_id: &str, home: Option<String>) {
        let Ok(author) = ActorId::from_hex(actor_id) else {
            return;
        };
        let action = self.state.borrow_mut().press();
        if action == FollowPress::Arm {
            self.repaint();
            return;
        }
        let follow = action == FollowPress::Follow;
        if let Some(btn) = self.btn.upgrade() {
            btn.set_sensitive(false);
        }
        let nest = client.nest_rpc().clone();
        // The follower's own identity seed → the subscriber keypair whose ML-KEM
        // ek gets published unconditionally (surface B, S4b).
        let subscriber_secret = client.secret_bytes();
        let this = Rc::clone(self);
        spawn_with_snapshot(
            &client.runtime_handle(),
            move || async move {
                let subs = SubscriptionsClient::new(nest);
                let subscriber = fauna_core::identity::ActorKeypair::from_secret(subscriber_secret);
                let ok = if follow {
                    subs.subscribe_across_nests(author, FOLLOWERS_TIER, &subscriber, home)
                        .await
                        .is_ok()
                } else {
                    subs.unsubscribe_across_nests(author, FOLLOWERS_TIER, &subscriber, home)
                        .await
                        .is_ok()
                };
                let status = if ok {
                    fetch_follow_status(&subs, author).await
                } else {
                    None
                };
                (ok, status)
            },
            move |(ok, status)| {
                if let Some(btn) = this.btn.upgrade() {
                    btn.set_sensitive(true);
                }
                if ok {
                    this.state.borrow_mut().replied(follow, status);
                }
                this.repaint();
            },
        );
    }
}

/// Start DM (`profile.md` § Where logic lives → Start DM action): seed the
/// Conversations new-thread composer with this actor as a recipient chip, then
/// switch to the Conversations page (`on_open_conversations`). Pure client nav
/// glue — `start_new_conversation` + `accept_new_thread_chip` are sync manager
/// mutations (no wire op; the group bootstraps lazily on first send). The chip
/// carries the real `actor_id`; its display handle is the actor_id hex (the
/// OTHER-profile header has no cached handle, same fallback as the header label).
fn start_dm(actor_id: &str, on_open_conversations: &Rc<dyn Fn()>) {
    let mgr = crate::conversations::manager();
    mgr.start_new_conversation();
    if let Ok(parsed) = ActorId::from_hex(actor_id) {
        mgr.accept_new_thread_chip(TypedAddress::Fauna {
            handle: actor_id.to_string(),
            actor_id: parsed,
        });
    }
    on_open_conversations();
}

/// The Block ⇄ Unblock toggle as built: "Block" label, but INERT until the
/// open-time edge read lands (or the viewer's own press authors the edge) — a
/// press on the not-blocked default would re-send `knocks_block` against an edge
/// the viewer already holds. [`apply_block_read`] enables it; mirrors tui's
/// `is_blocked: None`.
fn new_block_button() -> gtk::Button {
    let btn = gtk::Button::with_label(p::BLOCK);
    btn.add_css_class("destructive-action");
    btn.set_sensitive(false);
    set_test_id(&btn, ids::PROFILE_BLOCK_BUTTON);
    btn
}

/// Reflect the current block state on the toggle button: `blocked` → "Unblock"
/// (constructive), not-blocked → "Block" (destructive-styled). One place so the
/// initial read and every successful toggle render identically.
fn apply_block_state(btn: &gtk::Button, blocked: bool) {
    // The toggle label decision (`blocked` → "Unblock", else "Block") lives in
    // shared Rust (`fauna_core::format::contact_toggle_block_label`); the
    // `destructive-action` style stays an idiomatic per-app render.
    btn.set_label(
        &fauna_core::format::contact_toggle_block_label(blocked)
            .resolve(crate::i18n::strings::lookup),
    );
    if blocked {
        btn.remove_css_class("destructive-action");
    } else {
        btn.add_css_class("destructive-action");
    }
}

/// Apply an open-time block-state read to the toggle, UNLESS the viewer's own
/// press has already authored `is_blocked` for this open (`block_user_authored`).
///
/// `refresh_block_state` fires `fauna.contacts.list` when the page is built and
/// folds in whatever it returns — a full WS round-trip later, routinely AFTER
/// the viewer's first press. Applying that reply unconditionally rewound
/// `is_blocked` to the pre-press value, so the *next* press re-issued the same
/// op: it succeeded on the wire (no error to show), but the label never moved —
/// a user could block but never unblock. The read is hydration, authoritative
/// only until a press decides the field for real (
/// mirrors tui's `git log --grep "the block toggle's open-time read"`).
///
/// Linux rebuilds the whole profile view per open, so — unlike tui — there is no
/// cross-open half to guard: a stale reply from a PRIOR open closed over the old
/// `is_blocked`/`block_user_authored` pair and cannot reach a rebuilt view.
///
/// The toggle is built insensitive and this read is what enables it (a failed
/// read lands as `false`, so the toggle never stays stuck off): the edge is
/// not-yet-known until then, and a press on the not-blocked default would
/// re-send `knocks_block` against an edge the viewer already holds.
fn apply_block_read(
    btn: &gtk::Button,
    is_blocked: &Rc<Cell<bool>>,
    block_user_authored: &Rc<Cell<bool>>,
    blocked: bool,
) {
    btn.set_sensitive(true);
    if block_user_authored.get() {
        return;
    }
    is_blocked.set(blocked);
    apply_block_state(btn, blocked);
}

/// Read the viewed actor's current contact edge (`fauna.contacts.list`) and set
/// the toggle's initial state: a `blocked` edge → the button reads "Unblock"
/// (`contacts.md` § Persistence — the roster row carries `status`; match the hex
/// `peer_id`). Any other/absent status leaves the synchronous "Block" default.
/// The reply is dropped, not applied, once the viewer's own press has authored
/// `is_blocked` for this open — see [`apply_block_read`].
fn refresh_block_state(
    client: &Rc<FaunaClient>,
    actor_id: &str,
    btn: &gtk::Button,
    is_blocked: &Rc<Cell<bool>>,
    block_user_authored: &Rc<Cell<bool>>,
) {
    let nest = client.nest_rpc().clone();
    let peer_id = actor_id.to_string();
    let btn = btn.clone();
    let is_blocked = Rc::clone(is_blocked);
    let block_user_authored = Rc::clone(block_user_authored);
    spawn_with_snapshot(
        &client.runtime_handle(),
        move || async move {
            let contacts = fauna_client_contacts::ContactsClient::new(nest);
            match contacts.contacts_list().await {
                Ok(reply) => reply.contacts.iter().any(|c| {
                    fauna_core::format::contact_row_blocks_actor(&c.peer_id, &c.status, &peer_id)
                }),
                Err(_) => false,
            }
        },
        move |blocked| {
            apply_block_read(&btn, &is_blocked, &block_user_authored, blocked);
        },
    );
}

/// Block ⇄ unblock toggle (`profile.md` § Where logic lives → block; `contacts.md`
/// § Where logic lives → Unblock). On a not-blocked edge the tap calls the shared
/// `fauna_client_contacts::knocks_block` over `fauna.knocks.block` (upsert →
/// `blocked`); on a blocked edge it calls `knocks_unblock` over
/// `fauna.knocks.unblock` (the guarded clear-the-edge, `ContactStatus` → `None`).
/// The button is disabled during the round-trip and re-rendered from the new state
/// on success; a failure re-enables for a retry without changing state. A success
/// marks `block_user_authored` so a still-in-flight open-time read can no longer
/// rewind it — see [`apply_block_read`].
fn toggle_block(
    client: &Rc<FaunaClient>,
    actor_id: &str,
    btn: &gtk::Button,
    is_blocked: &Rc<Cell<bool>>,
    block_user_authored: &Rc<Cell<bool>>,
) {
    btn.set_sensitive(false);
    let was_blocked = is_blocked.get();
    let nest = client.nest_rpc().clone();
    let peer_id = actor_id.to_string();
    let btn = btn.clone();
    let is_blocked = Rc::clone(is_blocked);
    let block_user_authored = Rc::clone(block_user_authored);
    spawn_with_snapshot(
        &client.runtime_handle(),
        move || async move {
            let contacts = fauna_client_contacts::ContactsClient::new(nest);
            if was_blocked {
                contacts.knocks_unblock(peer_id).await.is_ok()
            } else {
                contacts.knocks_block(peer_id).await.is_ok()
            }
        },
        move |ok| {
            btn.set_sensitive(true);
            if ok {
                let now_blocked = !was_blocked;
                is_blocked.set(now_blocked);
                block_user_authored.set(true);
                apply_block_state(&btn, now_blocked);
            }
        },
    );
}

/// Re-render the header identity from the published `Profile`: the shared
/// `ProfileClient::profile_header_read` (this nest first, and on its
/// `not_found` the relay to the page's actor-home slot, `profile.md` § Where
/// logic lives → *The remote read*), decoded
/// (`fauna_client_profile::decode_profile_display`), re-painting the header's
/// names over the published `display_name`; on `not_found`/transport error they
/// keep their fallback ([`HeaderNames`]) and nothing reaches `error-message`.
/// The name paints first; then the pictures the profile names are fetched
/// through the session's post-blob host pick from the nest that served it
/// ([`picture_home`]) and painted ([`HeaderPictures`]) — tui's
/// `fetch_pictures`. Every call (a save included) refetches both.
///
/// `home` (OTHER only) is the page's actor-home slot with the hint the open
/// carried (and the Tiers tab's re-read hook, run after the slot is written): the slot as the open seeded it is where the relay goes, and the
/// SAME read replaces it through the shared rule ([`actor_home_for`]) — the
/// profile the header renders is the one whose home a follow or knock from this
/// page goes to.
fn refresh_header_name(
    client: &Rc<FaunaClient>,
    names: &HeaderNames,
    pictures: &HeaderPictures,
    actor_id: &str,
    home: Option<HeaderHome>,
) {
    let nest = client.nest_rpc().clone();
    let actor_id = actor_id.to_string();
    let own_nest_url = client.node_url().to_string();
    let names = names.clone();
    let pictures = pictures.clone();
    let blob_client = Rc::clone(client);
    let painted = actor_id.clone();
    let relay_home = home.as_ref().and_then(|h| h.slot.borrow().clone());
    let carried = home.as_ref().and_then(|h| h.carried.clone());
    let is_other = home.is_some();
    spawn_with_snapshot(
        &client.runtime_handle(),
        move || async move {
            // The OTHER header holds no handle: re-read the contact roster now, so
            // a contact accepted a moment ago is named by the roster's handle
            // rather than the short id until the receive loop's next tick
            // (`value-formatting.md` § Peer display label → *the roster handle*).
            if is_other {
                crate::conversations::manager()
                    .refresh_contact_roster()
                    .await;
            }
            let pc = ProfileClient::new(nest);
            let (body, home) = match pc
                .profile_header_read(actor_id.clone(), relay_home.as_deref())
                .await
            {
                Ok(read) => (
                    Some(read.reply.body.to_vec()),
                    picture_home(
                        read.relayed_from.as_deref(),
                        read.home_nest_actor_id.as_deref(),
                    ),
                ),
                Err(_) => (None, None),
            };
            let display = body
                .as_deref()
                .and_then(|body| decode_profile_display(body).ok());
            let name = display
                .as_ref()
                .and_then(|d| d.display_name.clone())
                .filter(|s| !s.trim().is_empty());
            let named = HeaderPictureHashes {
                avatar: display.as_ref().and_then(|d| d.avatar_hash_hex()),
                banner: display.as_ref().and_then(|d| d.banner_hash_hex()),
                home,
            };
            let resolved = is_other
                .then(|| {
                    actor_home_for(
                        carried.as_deref(),
                        &actor_id,
                        body.as_deref(),
                        &own_nest_url,
                    )
                })
                .flatten();
            (name, resolved, named)
        },
        move |(display_name, resolved, named)| {
            *names.display_name.borrow_mut() = display_name;
            names.paint(&painted);
            if let Some(h) = home {
                *h.slot.borrow_mut() = resolved;
                // The OTHER Tiers tab re-reads its offers if they were read
                // under the home this read just replaced.
                let moved = h.moved.borrow().clone();
                if let Some(moved) = moved {
                    moved();
                }
            }
            fetch_header_pictures(&blob_client, &pictures, named);
        },
    );
}

/// What the OTHER header read needs of the page's actor home: the slot it
/// reads and replaces, the hint the open carried, and the hook the Tiers tab
/// sets so a replaced slot re-reads the offers (filled once the tab is built).
#[derive(Clone)]
struct HeaderHome {
    slot: HomeSlot,
    carried: Option<String>,
    moved: HomeMovedHook,
}

/// The OTHER Tiers tab's `home_moved` entry point, set after the tab is built.
type HomeMovedHook = Rc<RefCell<Option<Rc<dyn Fn()>>>>;

/// The pictures a header read found named: their hex blob hashes, and the
/// nest whose blob route serves them ([`picture_home`]).
struct HeaderPictureHashes {
    avatar: Option<String>,
    banner: Option<String>,
    home: Option<fauna_client::ForeignHome>,
}

/// Render the elements `named` names, then fetch each picture through the
/// session's post-blob host pick — capped and verified against the hash
/// ([`fauna_client::PostBlobHosts::get`]) — and paint it. A failed fetch
/// leaves its element empty, never `error-message`.
fn fetch_header_pictures(
    client: &Rc<FaunaClient>,
    pictures: &HeaderPictures,
    named: HeaderPictureHashes,
) {
    let epoch = pictures.name(named.avatar.is_some(), named.banner.is_some());
    for (hash, picture) in [
        (named.avatar, pictures.avatar.clone()),
        (named.banner, pictures.banner.clone()),
    ] {
        let Some(hash) = hash else { continue };
        let rx = client.fetch_post_blob_bytes(&hash, named.home.clone());
        let pictures = pictures.clone();
        gtk::glib::spawn_future_local(async move {
            if let Ok(Ok(bytes)) = rx.recv().await {
                pictures.paint(epoch, &picture, &bytes);
            }
        });
    }
}

/// The OTHER actor's home for `actor_id_hex` through the shared
/// `fauna_client_profile::actor_home` rule: their own signed profile (the body
/// the header read fetched, `None` before it answers) beats the hint the open
/// carried; a hint naming the viewer's own nest is this nest. A malformed id
/// answers `None` (this nest) like any other unusable input.
fn actor_home_for(
    carried: Option<&str>,
    actor_id_hex: &str,
    profile_body: Option<&[u8]>,
    own_nest_url: &str,
) -> Option<String> {
    let actor = ActorId::from_hex(actor_id_hex).ok()?;
    fauna_client_profile::actor_home(carried, &actor, profile_body, own_nest_url)
}

/// The OTHER profile's `profile-request-contact-button` plus its guardian-ask
/// pair — the client-free half, so a unit test can drive the render.
fn build_request_contact(actor_id: &str) -> (gtk::Button, Rc<GuardianAsk>) {
    let btn = gtk::Button::with_label(p::REQUEST_CONTACT);
    set_test_id(&btn, ids::PROFILE_REQUEST_CONTACT_BUTTON);
    // The knock rides `fauna.inbox.send` — tui's `Action::RequestContact`.
    crate::offline_gate::declare_wire_kind(&btn, "fauna.inbox.send");
    (btn, GuardianAsk::new(actor_id))
}

/// Wire the knock and the ask to the nest. Each reply closes over THIS open's
/// widgets (linux rebuilds the whole view per open), so a reply that outlives
/// its open lands on detached widgets and can never paint "Sent", or offer the
/// ask, on the next actor (rule (g)).
fn wire_request_contact(
    client: &Rc<FaunaClient>,
    actor_id: &str,
    btn: &gtk::Button,
    ask: &Rc<GuardianAsk>,
    error_label: &gtk::Label,
    knock_route: &HomeSlot,
) {
    {
        let client = Rc::clone(client);
        let actor_id = actor_id.to_string();
        let ask = Rc::clone(ask);
        let error_label = error_label.clone();
        let knock_route = Rc::clone(knock_route);
        btn.connect_clicked(move |btn| {
            btn.set_sensitive(false);
            let btn = btn.clone();
            let ask = Rc::clone(&ask);
            let error_label = error_label.clone();
            let route = knock_route.borrow().clone();
            client.send_knock_classified(&actor_id, route, move |outcome| {
                let error = apply_knock_outcome(&btn, &ask, outcome);
                crate::settings::render_error_label(&error_label, error.as_deref());
            });
        });
    }
    {
        let client = Rc::clone(client);
        let actor_id = actor_id.to_string();
        let weak = Rc::downgrade(ask);
        let error_label = error_label.clone();
        ask.connect_ask(move || {
            let weak = weak.clone();
            let error_label = error_label.clone();
            client.ask_guardian_for_contact(&actor_id, move |result| {
                let Some(ask) = weak.upgrade() else { return };
                match result {
                    Ok(requests) => {
                        ask.ask_landed(requests);
                        crate::settings::render_error_label(&error_label, None);
                    }
                    Err(e) => {
                        ask.ask_failed();
                        crate::settings::render_error_label(&error_label, Some(&e));
                    }
                }
            });
        });
    }
}

/// Fold one classified knock outcome into this open's button + ask pair, and
/// answer what `error-message` should now say (`None` = clear it). `Sent` reads
/// "Request sent" and stays disabled (a sent knock is not re-sent); the TYPED
/// guardian refusal re-enables, offers the ask, and STAYS on `error-message`
/// (rule (b)); any other failure re-enables and offers nothing (rule (a)).
fn apply_knock_outcome(btn: &gtk::Button, ask: &GuardianAsk, outcome: KnockSend) -> Option<String> {
    match outcome {
        // `Queued` folds as a sent knock (the identity drain sends it); linux
        // lends no outbox to its knock send yet, so it never reaches here.
        KnockSend::Sent | KnockSend::Queued => {
            btn.set_label(p::REQUEST_CONTACT_SENT);
            btn.set_sensitive(false);
            None
        }
        KnockSend::RefusedByGuardian => {
            btn.set_sensitive(true);
            ask.refused();
            Some(contacts_strings::GUARDIAN_APPROVAL_REQUIRED.to_string())
        }
        KnockSend::Failed(e) => {
            btn.set_sensitive(true);
            Some(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A LATE open-time read must not rewind the edge the viewer's own press
    /// already authored. `refresh_block_state` issues its `contacts.list` read
    /// when the page is built; over a real WS round-trip that answer routinely
    /// lands AFTER the viewer's first press, and applying it unconditionally
    /// used to rewind `is_blocked` to the pre-press value (
    /// mirrors tui's `a_late_open_hydration_does_not_rewind_the_user_toggle`).
    #[test]
    fn a_late_open_time_read_does_not_rewind_the_users_press() {
        crate::testid::run_on_gtk_thread(|| {
            let btn = gtk::Button::with_label("placeholder");
            let is_blocked = Rc::new(Cell::new(false));
            let block_user_authored = Rc::new(Cell::new(false));

            // The viewer's own press: blocks, and authors the edge for this open.
            is_blocked.set(true);
            block_user_authored.set(true);
            apply_block_state(&btn, true);
            assert_eq!(btn.label().as_deref(), Some(p::UNBLOCK));

            // The open-time read, issued BEFORE that press, finally returns —
            // still reporting not-blocked.
            apply_block_read(&btn, &is_blocked, &block_user_authored, false);

            assert_eq!(
                btn.label().as_deref(),
                Some(p::UNBLOCK),
                "a hydration read issued before the press must not rewind the \
                 edge the press authored"
            );
        });
    }

    /// The consequence the e2e would eventually catch: with `is_blocked`
    /// rewound by a late read, the viewer's SECOND press re-issues
    /// `knocks_block` (`toggle_block` reads `is_blocked.get()` to choose the
    /// op) instead of unblocking — succeeding on the wire, with nothing to
    /// error about, and the label stuck on "Unblock" forever. This asserts the
    /// state the next `toggle_block` call would read, without driving the
    /// network round-trip itself.
    #[test]
    fn a_late_read_leaves_is_blocked_true_for_the_next_press() {
        crate::testid::run_on_gtk_thread(|| {
            let btn = gtk::Button::with_label("placeholder");
            let is_blocked = Rc::new(Cell::new(false));
            let block_user_authored = Rc::new(Cell::new(false));

            is_blocked.set(true);
            block_user_authored.set(true);
            apply_block_read(&btn, &is_blocked, &block_user_authored, false);

            assert!(
                is_blocked.get(),
                "the next press must issue knocks_unblock against the blocked \
                 edge; a rewound value would re-issue knocks_block instead"
            );
        });
    }

    /// Between open and the open-time edge read the toggle is inert: the edge is
    /// not-yet-known, and a press on the not-blocked default would re-send
    /// `knocks_block` against an edge the viewer already holds (mirrors tui's
    /// `the_toggle_is_inert_until_the_open_time_read_lands`). The read enables it
    /// — including a failed read, which lands as not-blocked.
    #[test]
    fn the_toggle_is_inert_until_the_open_time_read_lands() {
        crate::testid::run_on_gtk_thread(|| {
            let view = new_block_button();
            assert!(
                !view.is_sensitive(),
                "the toggle is disabled until the edge is known"
            );

            let is_blocked = Rc::new(Cell::new(false));
            let block_user_authored = Rc::new(Cell::new(false));
            apply_block_read(&view, &is_blocked, &block_user_authored, true);
            assert!(view.is_sensitive(), "the read enables the toggle");
            assert!(is_blocked.get());
        });
    }

    /// A profile view carries its own fresh `is_blocked`/`block_user_authored`
    /// pair per build (unlike tui's long-lived page state, linux rebuilds the
    /// whole view per open), so an ordinary unauthored open must still accept
    /// its read — the guard must not itself become a stuck "Block" default.
    #[test]
    fn an_unauthored_open_time_read_still_applies() {
        crate::testid::run_on_gtk_thread(|| {
            let btn = gtk::Button::with_label("placeholder");
            let is_blocked = Rc::new(Cell::new(false));
            let block_user_authored = Rc::new(Cell::new(false));

            apply_block_read(&btn, &is_blocked, &block_user_authored, true);

            assert!(is_blocked.get(), "an unauthored open must accept the read");
            assert_eq!(btn.label().as_deref(), Some(p::UNBLOCK));
        });
    }

    fn mounted(btn: &gtk::Button, ask: &GuardianAsk) -> gtk::Box {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(btn);
        root.append(ask.widget());
        root
    }

    fn count(root: &gtk::Box, id: &str) -> usize {
        crate::automation::find::count_in(root.upcast_ref(), id)
    }

    /// `profile-request-contact-button` reads "Request sent" and stays
    /// disabled once the nest accepted the knock — a sent knock is not re-sent
    /// (tui's `the_knock_goes_to_the_viewed_actor_on_its_profiles_route`,
    /// render half; the route itself is the shared
    /// `knock_recipient_nest_url`, pinned in `fauna-client-profile`).
    #[test]
    fn a_sent_knock_reads_request_sent_and_stays_disabled() {
        crate::testid::run_on_gtk_thread(|| {
            crate::ward_asks::clear_for_identity_change();
            let (btn, ask) = build_request_contact(&"cd".repeat(32));
            assert_eq!(btn.label().as_deref(), Some(p::REQUEST_CONTACT));
            btn.set_sensitive(false); // in flight
            assert_eq!(apply_knock_outcome(&btn, &ask, KnockSend::Sent), None);
            assert_eq!(btn.label().as_deref(), Some(p::REQUEST_CONTACT_SENT));
            assert!(!btn.is_sensitive());
        });
    }

    /// Only the TYPED refusal reveals `contact-request-guardian-button` (a
    /// transport failure must not imply supervision), and the refusal stays
    /// on `error-message`. Mirrors tui's profile arm of the same name.
    #[test]
    fn a_guardian_refused_knock_offers_the_ask() {
        crate::testid::run_on_gtk_thread(|| {
            crate::ward_asks::clear_for_identity_change();
            let (btn, ask) = build_request_contact(&"cd".repeat(32));
            let root = mounted(&btn, &ask);
            assert_eq!(count(&root, ids::CONTACT_REQUEST_GUARDIAN_BUTTON), 0);

            let err = apply_knock_outcome(&btn, &ask, KnockSend::Failed("inbox send: boom".into()));
            assert_eq!(err.as_deref(), Some("inbox send: boom"));
            assert_eq!(
                count(&root, ids::CONTACT_REQUEST_GUARDIAN_BUTTON),
                0,
                "a transport failure must not imply supervision"
            );

            let err = apply_knock_outcome(&btn, &ask, KnockSend::RefusedByGuardian);
            assert_eq!(
                err.as_deref(),
                Some(contacts_strings::GUARDIAN_APPROVAL_REQUIRED)
            );
            assert_eq!(count(&root, ids::CONTACT_REQUEST_GUARDIAN_BUTTON), 1);
            assert_eq!(btn.label().as_deref(), Some(p::REQUEST_CONTACT));
        });
    }

    /// Rule (g): a knock reply that outlived its open answers the actor you
    /// LEFT. Each open builds its own pair, so the reply lands on the old one
    /// and the new open neither reads "Sent" nor offers the ask.
    #[test]
    fn a_knock_reply_for_a_previous_open_does_not_paint_on_this_one() {
        crate::testid::run_on_gtk_thread(|| {
            crate::ward_asks::clear_for_identity_change();
            let (left_btn, left_ask) = build_request_contact(&"cd".repeat(32));
            let (btn, ask) = build_request_contact(&"ef".repeat(32));
            let root = mounted(&btn, &ask);

            apply_knock_outcome(&left_btn, &left_ask, KnockSend::RefusedByGuardian);
            apply_knock_outcome(&left_btn, &left_ask, KnockSend::Sent);

            assert_eq!(count(&root, ids::CONTACT_REQUEST_GUARDIAN_BUTTON), 0);
            assert_eq!(btn.label().as_deref(), Some(p::REQUEST_CONTACT));
            assert!(btn.is_sensitive());
        });
    }

    /// A malformed actor id routes nowhere foreign — `None` = this nest.
    #[test]
    fn a_malformed_actor_id_knocks_on_this_nest() {
        assert_eq!(
            actor_home_for(
                Some("https://met.test"),
                "not-hex",
                Some(b"".as_slice()),
                "https://home.test"
            ),
            None
        );
    }

    /// The carried hint seeds the slot until the header read answers; a hint
    /// naming the viewer's own nest is this nest (the shared rule's pins).
    #[test]
    fn the_carried_home_seeds_the_actor_home_slot() {
        let actor = "ab".repeat(32);
        assert_eq!(
            actor_home_for(Some("https://met.test"), &actor, None, "https://home.test"),
            Some("https://met.test".to_string())
        );
        assert_eq!(
            actor_home_for(
                Some("https://HOME.test/"),
                &actor,
                None,
                "https://home.test"
            ),
            None
        );
        assert_eq!(
            actor_home_for(None, &actor, None, "https://home.test"),
            None
        );
    }

    /// A 1×1 PNG, the bytes a fetched picture decodes from.
    fn png() -> Vec<u8> {
        let texture = gtk::gdk::MemoryTexture::new(
            1,
            1,
            gtk::gdk::MemoryFormat::R8g8b8a8,
            &gtk::glib::Bytes::from_static(&[0xff, 0, 0, 0xff]),
            4,
        );
        texture.save_to_png_bytes().to_vec()
    }

    /// `profile-avatar` / `profile-banner` render only while the profile
    /// names the picture, empty until its bytes paint (tui's
    /// `the_header_paints_only_the_pictures_the_profile_names`); the generic
    /// avatar icon stands in for a profile with no avatar.
    #[test]
    fn the_header_paints_only_the_pictures_the_profile_names() {
        crate::testid::run_on_gtk_thread(|| {
            let pictures = HeaderPictures::new();
            assert!(!pictures.avatar.is_visible() && !pictures.banner.is_visible());
            assert!(pictures.avatar_icon.is_visible());

            let epoch = pictures.name(true, false);
            assert!(pictures.avatar.is_visible(), "a named avatar renders");
            assert!(!pictures.banner.is_visible(), "an unnamed banner does not");
            assert!(!pictures.avatar_icon.is_visible());
            assert!(pictures.avatar.paintable().is_none(), "empty until painted");

            pictures.paint(epoch, &pictures.avatar, b"not an image");
            assert!(
                pictures.avatar.paintable().is_none(),
                "undecodable bytes stay empty"
            );
            pictures.paint(epoch, &pictures.avatar, &png());
            assert!(pictures.avatar.paintable().is_some());

            pictures.name(false, false);
            assert!(
                !pictures.avatar.is_visible(),
                "a cleared avatar leaves the header"
            );
            assert!(pictures.avatar.paintable().is_none());
            assert!(pictures.avatar_icon.is_visible());
        });
    }

    /// A fetch an older header read started never paints over a newer read's
    /// pictures — a save refetches both, and the first fetch may land last.
    #[test]
    fn a_stale_picture_fetch_does_not_paint() {
        crate::testid::run_on_gtk_thread(|| {
            let pictures = HeaderPictures::new();
            let stale = pictures.name(true, true);
            let fresh = pictures.name(true, true);
            pictures.paint(stale, &pictures.banner, &png());
            assert!(pictures.banner.paintable().is_none());
            pictures.paint(fresh, &pictures.banner, &png());
            assert!(pictures.banner.paintable().is_some());
        });
    }

    /// The pictures come from the nest that served the profile: this one for a
    /// local read, the relayed-from home otherwise, pinned to the identity the
    /// relay stamped (tui's `the_pictures_come_from_the_nest_that_served_the_profile`).
    #[test]
    fn the_pictures_come_from_the_nest_that_served_the_profile() {
        assert_eq!(picture_home(None, None), None);
        let id = "ab".repeat(32);
        assert_eq!(
            picture_home(Some("https://home.test"), Some(&id)),
            Some(fauna_client::ForeignHome::new(
                "https://home.test",
                Some(&id)
            ))
        );
        assert_eq!(
            picture_home(Some("https://home.test"), None),
            Some(fauna_client::ForeignHome::new("https://home.test", None)),
            "an unstamped relay keeps the WebPKI floor"
        );
    }
}

#[cfg(test)]
mod follow_toggle_tests {
    use super::{FollowPress, FollowState, FollowStatus, p};

    fn holding(tier: Option<&str>) -> FollowState {
        FollowState {
            status: Some(FollowStatus {
                tier: tier.map(str::to_string),
                follow_pending: false,
            }),
            ..FollowState::default()
        }
    }

    #[test]
    fn a_paid_holder_reads_following_and_needs_two_presses() {
        // The bug the shared derivation closes: a paid (`gold`) holder follows.
        let mut st = holding(Some("gold"));
        assert_eq!(st.label(), p::FOLLOWING);
        assert_eq!(st.press(), FollowPress::Arm);
        assert_eq!(st.label(), p::UNFOLLOW_CONFIRM);
        assert_eq!(st.press(), FollowPress::Unfollow);
        st.replied(false, holding(Some("gold")).status);
        // The optimistic flip outranks a status read still naming the tier.
        assert_eq!(st.label(), p::FOLLOW);
        assert!(!st.armed);
    }

    #[test]
    fn a_free_follower_unfollows_at_once_and_a_stranger_follows() {
        let mut st = holding(Some("followers"));
        assert_eq!(st.press(), FollowPress::Unfollow);
        let mut stranger = holding(None);
        assert_eq!(stranger.label(), p::FOLLOW);
        assert_eq!(stranger.press(), FollowPress::Follow);
        stranger.replied(true, None);
        assert_eq!(stranger.label(), p::FOLLOWING);
        // No status read yet → Follow.
        assert_eq!(FollowState::default().label(), p::FOLLOW);
    }
}
