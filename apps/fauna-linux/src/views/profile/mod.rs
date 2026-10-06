//! The Profile page — the canonical per-user *detail* surface (`profile.md`).
//! It renders **either** the viewer's own profile (SELF, reached via the
//! top-level `profile-tab`) **or** another actor's (reached by tap-through —
//! e.g. a contact row), branching on `is_self` (the `target` argument). Both
//! show an identity header (`user-header`) + a tab strip (`profile-posts-tab`
//! landmark · `profile-tiers-tab`). The Tiers tab hosts the subscriptions
//! Slice-A author management (`tiers`) on the SELF profile, or the Slice-B
//! subscriber-browse offers section (`offers`) on another's.
//!
//! The header renders the actor's **display_name** when they have published a
//! `Profile` (`fauna.profile.get` → `fauna_client_profile::decode_profile`),
//! falling back to the **handle** (account cache, SELF only) then the actor_id.
//! On another's profile the name goes through the one shared resolver over the
//! viewer's own nickname for them (`value-formatting.md` § Peer display label):
//! a nickname heads the page and the public name it replaced stays beneath it
//! (`profile-public-name`), and the **private section** ([`private`]) edits
//! that nickname, the notes and the labels.
//! The avatar is still a placeholder (avatar/banner are deferred). The primary
//! relationship action is `profile-edit-button` (own profile → the text-only
//! edit form `edit`, publishing via `fauna.profile.set`) or
//! `profile-follow-button` (another's → subscribe to the free "followers" tier).

pub mod edit;
pub mod offers;
pub mod private;
pub mod tiers;

use fauna_ui_ids as ids;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;

use fauna_client_profile::{ProfileClient, decode_profile};
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

/// Handles the app retains. The page refreshes itself on each mutation
/// (observer-free, `feed.md` § Architectural rules); `refresh_tiers` additionally
/// lets `app.rs` re-read the Tiers-tab sections when the profile page becomes
/// visible (mirroring the Peers page's on-visible refresh), so a pending
/// subscribe request that arrived while the author was elsewhere shows up.
pub struct ProfileHandles {
    pub refresh_tiers: Rc<dyn Fn()>,
}

/// Build the Profile page. `target` is the viewed actor: `None` = the viewer's
/// own profile (SELF — reached via the `profile-tab` sidebar row); `Some(hex)` =
/// another actor's profile, reached by tap-through (`profile.md` § Layout &
/// flow). Everything below branches on `is_self`: the header shows the
/// `profile-edit-button` (self) or `profile-follow-button` (other), and the
/// Tiers tab hosts the SELF author-management sections (`tiers`) or the OTHER
/// subscriber-browse offers section (`offers`). The page is rebuilt per target
/// by `app.rs`'s `open_profile`.
pub fn build_profile_view(
    client: &Rc<FaunaClient>,
    target: Option<String>,
    on_open_conversations: Rc<dyn Fn()>,
) -> (gtk::Box, ProfileHandles) {
    let is_self = target.is_none();
    let actor_id = target.unwrap_or_else(|| client.actor_id().unwrap_or_default());

    let outer = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .build();
    set_test_id(&outer, ids::PROFILE_VIEW);

    let (header, header_widgets) = build_header(client, &actor_id, is_self);
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

    // OTHER: where `profile-request-contact-button`'s knock goes — the shared
    // `fauna_client_profile::knock_recipient_nest_url` over the profile this
    // open's header read already fetched (`profile.md` § Where logic lives →
    // *Request contact routing*). `None` until that read lands, and whenever
    // the profile gives no better answer: the knock then goes to this nest.
    let knock_route: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

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
        let block_btn = gtk::Button::with_label(p::BLOCK);
        block_btn.add_css_class("destructive-action");
        set_test_id(&block_btn, ids::PROFILE_BLOCK_BUTTON);
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
            &knock_route,
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
    let refresh_header: Rc<dyn Fn()> = {
        let client = Rc::clone(client);
        let names = header_widgets.names.clone();
        let actor_id = actor_id.clone();
        // SELF has no one to knock, so no route slot to fill.
        let route_slot = (!is_self).then(|| Rc::clone(&knock_route));
        Rc::new(move || refresh_header_name(&client, &names, &actor_id, route_slot.clone()))
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

    // Inner stack: posts (landmark, content TBD) / tiers (SELF author-management
    // sections, or — for another actor — the subscriber-browse offers section).
    let stack = gtk::Stack::new();
    stack.set_vexpand(true);

    let posts_page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .build();
    let posts_placeholder = gtk::Label::new(Some(p::NO_POSTS));
    posts_placeholder.set_halign(gtk::Align::Start);
    posts_placeholder.add_css_class("dim-label");
    posts_placeholder.set_margin_start(12);
    posts_page.append(&posts_placeholder);
    stack.add_named(&posts_page, Some("posts"));

    let (tiers_page, refresh_tiers) = if is_self {
        tiers::build_tiers_tab(client, error_label.clone())
    } else {
        // Another actor's offered tiers (subscriber browse). A malformed target
        // hex degrades to an empty offers list rather than panicking.
        let author = ActorId::from_hex(&actor_id).unwrap_or(ActorId([0u8; 32]));
        offers::build_offers_tab(client, error_label.clone(), author)
    };
    stack.add_named(&tiers_page, Some("tiers"));
    stack.set_visible_child_name("posts");
    outer.append(&stack);

    {
        let stack = stack.clone();
        let heading = heading.clone();
        posts_tab.connect_clicked(move |_| {
            stack.set_visible_child_name("posts");
            heading.set_text(p::POSTS);
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

    (outer, ProfileHandles { refresh_tiers })
}

/// Header widgets the page wires after construction: the identity `names`
/// (refreshed with the published display_name) and — SELF only — the `edit_btn`
/// (opens the edit form). On another actor's profile the `profile-follow-button`
/// is built and wired here (no `edit_btn`), so `edit_btn` is `None`.
struct HeaderWidgets {
    names: HeaderNames,
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
    /// `Some(fallback)` on the SELF profile — the cached handle, else the
    /// actor id; `None` on another's, whose names the shared resolver answers.
    self_fallback: Option<String>,
}

impl HeaderNames {
    /// The two lines for `actor_id`: SELF = published display name → the
    /// fallback; OTHER = the one shared resolver over the viewer's nickname,
    /// the published display name, then the canonical short id (the OTHER
    /// header holds no handle).
    fn label(&self, actor_id: &str) -> fauna_core::format::PeerLabel {
        let display_name = self.display_name.borrow();
        match &self.self_fallback {
            Some(fallback) => fauna_core::format::PeerLabel {
                primary: display_name.clone().unwrap_or_else(|| fallback.clone()),
                public: None,
            },
            None => overlays::projection().peer_label(display_name.as_deref(), None, actor_id),
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
) -> (gtk::Box, HeaderWidgets) {
    let header = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .margin_top(12)
        .margin_start(12)
        .margin_end(12)
        .build();

    let avatar = gtk::Image::from_icon_name("avatar-default-symbolic");
    avatar.set_pixel_size(48);
    header.append(&avatar);

    let info = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .build();
    // Initial names: own handle (from the account cache) on the SELF profile,
    // else the resolver's answer with no published name yet.
    // `refresh_header_name` upgrades this to the published display_name once
    // `fauna.profile.get` returns.
    let self_fallback = is_self.then(|| {
        let (handle, _domain, _tier) = load_account_cache();
        handle
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| actor_id.to_string())
    });
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
        self_fallback,
    };
    names.paint(actor_id);

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
        // The follow-toggle label decision (`is_following` → "Following", else
        // "Follow") lives in shared Rust (`fauna_core::format::follow_toggle_label`);
        // this header has no cached follow status, so it starts from the
        // not-following arm and `follow()` flips it on success (optimistic).
        let follow_btn = gtk::Button::with_label(
            &fauna_core::format::follow_toggle_label(false).resolve(crate::i18n::strings::lookup),
        );
        follow_btn.set_valign(gtk::Align::Center);
        set_test_id(&follow_btn, ids::PROFILE_FOLLOW_BUTTON);
        crate::offline_gate::declare_wire_kind(&follow_btn, "fauna.subscriptions.subscribe");
        {
            let client = Rc::clone(client);
            let actor_id = actor_id.to_string();
            follow_btn.connect_clicked(move |btn| follow(&client, &actor_id, btn));
        }
        header.append(&follow_btn);
        None
    };

    (header, HeaderWidgets { names, edit_btn })
}

/// Follow = subscribe to the free "followers" tier (`profile.md` § Where logic
/// lives → *Follow / unfollow*). On success the button label flips to "Following".
fn follow(client: &Rc<FaunaClient>, actor_id: &str, btn: &gtk::Button) {
    let Ok(author) = ActorId::from_hex(actor_id) else {
        return;
    };
    btn.set_sensitive(false);
    let nest = client.nest_rpc().clone();
    // The follower's own identity seed → the subscriber keypair whose ML-KEM ek
    // gets published unconditionally (surface B, S4b).
    let subscriber_secret = client.secret_bytes();
    let btn = btn.clone();
    spawn_with_snapshot(
        &client.runtime_handle(),
        move || async move {
            let subs = SubscriptionsClient::new(nest);
            let subscriber = fauna_core::identity::ActorKeypair::from_secret(subscriber_secret);
            subs.subscribe_publishing_ek(author, FOLLOWERS_TIER, &subscriber)
                .await
                .is_ok()
        },
        move |ok| {
            btn.set_sensitive(true);
            if ok {
                btn.set_label(
                    &fauna_core::format::follow_toggle_label(true)
                        .resolve(crate::i18n::strings::lookup),
                );
            }
        },
    );
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
fn apply_block_read(
    btn: &gtk::Button,
    is_blocked: &Rc<Cell<bool>>,
    block_user_authored: &Rc<Cell<bool>>,
    blocked: bool,
) {
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

/// Re-render the header identity from the published `Profile`: fetch
/// `fauna.profile.get(actor_id)`, decode (`fauna_client_profile::decode_profile`),
/// and re-paint the header's names over the published `display_name`; on
/// `not_found`/transport error they keep their fallback ([`HeaderNames`]).
/// Works for any actor — the read is a public-by-actor_id fetch.
///
/// `knock_route` (OTHER only) is filled off the SAME read: the profile the
/// header renders is the one whose home nest a knock from this page goes to
/// ([`knock_route_for`]).
fn refresh_header_name(
    client: &Rc<FaunaClient>,
    names: &HeaderNames,
    actor_id: &str,
    knock_route: Option<Rc<RefCell<Option<String>>>>,
) {
    let nest = client.nest_rpc().clone();
    let actor_id = actor_id.to_string();
    let own_nest_url = client.node_url().to_string();
    let names = names.clone();
    let painted = actor_id.clone();
    spawn_with_snapshot(
        &client.runtime_handle(),
        move || async move {
            let pc = ProfileClient::new(nest);
            match pc.profile_get(actor_id.clone()).await {
                Ok(reply) => {
                    let body: &[u8] = reply.body.as_ref();
                    let name = decode_profile(body)
                        .ok()
                        .and_then(|(prof, _origin)| prof.display_name)
                        .filter(|s| !s.trim().is_empty());
                    (name, knock_route_for(&actor_id, body, &own_nest_url))
                }
                Err(_) => (None, None),
            }
        },
        move |(display_name, route)| {
            *names.display_name.borrow_mut() = display_name;
            names.paint(&painted);
            if let Some(slot) = knock_route {
                *slot.borrow_mut() = route;
            }
        },
    );
}

/// The knock route for `actor_id_hex` off the profile body the open fetched —
/// the shared `fauna_client_profile::knock_recipient_nest_url`; a malformed id
/// answers `None` (this nest) like any other unusable input.
fn knock_route_for(actor_id_hex: &str, profile_body: &[u8], own_nest_url: &str) -> Option<String> {
    let actor = ActorId::from_hex(actor_id_hex).ok()?;
    fauna_client_profile::knock_recipient_nest_url(&actor, profile_body, own_nest_url)
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
    knock_route: &Rc<RefCell<Option<String>>>,
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
        KnockSend::Sent => {
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
        assert_eq!(knock_route_for("not-hex", b"", "https://home.test"), None);
    }
}
