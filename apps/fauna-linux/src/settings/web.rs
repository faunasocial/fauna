//! User "Web" settings page (`web-settings`) — the per-user authoring surface
//! for web-content hosting (`docs/goal/behavior/web-content-hosting.md`).
//!
//! Two controls on one page (`web-content-hosting.md` § Published-post
//! management): the **subdomain opt-in toggle** (`web-settings-subdomain-toggle`,
//! default OFF) that serves the user's `web` content at
//! `https://<handle>.<domain>/`, and the **Published-posts management
//! section** — every post this actor published to the web
//! (`fauna.web.publish.list`), each offering copy-link / copy-paywall-link
//! (gated rows only) / unpublish. The live URL (or a disabled reason) renders
//! below the toggle, plus a static explainer pointing at the two content
//! sources (a `web`-mode folder + web-published posts). Renders off the
//! shared `fauna-client-web` crate so every app shows the same thing
//! (priorities #1/#2). The admin uses this same page for their own site; the
//! nest-wide apex designation is the separate `admin-web` page.

use fauna_ui_ids as ids;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use fauna_client::NestClient;
use fauna_client_web::{SiteLinkDisabledReason, SubdomainDisabledReason, WebClient, WebPageRead};
use fauna_protocol::web::PublishedPost;

use crate::async_helper::spawn_with_snapshot;
use crate::i18n::strings::web_publish as WP;
use crate::i18n::strings::web_settings as WS;
use crate::testid::{set_test_attr, set_test_id};

type WebRpc = WebClient<Arc<NestClient>>;

thread_local! {
    /// The last-hydrated `web-settings` read, shared with the feed ⋯-menu's
    /// web-publish verbs (`views/feed/post_list.rs`) so both surfaces resolve
    /// the SAME origin (`fauna_client_web::site_link_view`) instead of each
    /// growing its own fetch sequence (`web-content-hosting.md`
    /// § Published-post management). `None` until either surface has read it
    /// once this session; this page always overwrites it with its own fresh
    /// read, since a visit here is the authoritative re-hydrate.
    static CACHED_WEB_PAGE: RefCell<Option<WebPageRead>> = const { RefCell::new(None) };
    /// Guards [`ensure_hydrated`] against firing a second concurrent read —
    /// e.g. the feed ⋯-menu opened twice before the first lazy hydrate lands.
    static WEB_PAGE_HYDRATING: Cell<bool> = const { Cell::new(false) };
}

/// The site-link view resolved from the cached read, if one has landed this
/// session — `None` until either surface has read it once. `pub(crate)`: the
/// feed ⋯-menu's copy affordances (`views/feed/post_list.rs`) resolve their
/// origin from this, so the two surfaces can never disagree.
pub(crate) fn cached_site_link() -> Option<fauna_client_web::SiteLinkView> {
    CACHED_WEB_PAGE.with(|c| c.borrow().as_ref().map(resolve_site_link))
}

/// The canonical actor-scoped drop's call
/// (`crate::actor_scope::reset_actor_scoped_state`) — an account switch must
/// not let the incoming actor's copy-link verbs resolve against the outgoing
/// actor's origin (`account-scoping.md` § The scoping taxonomy → the
/// in-memory corollary).
pub(crate) fn clear_for_actor_change() {
    CACHED_WEB_PAGE.with(|c| *c.borrow_mut() = None);
    WEB_PAGE_HYDRATING.with(|f| f.set(false));
}

/// Ensure the cache is populated, for a caller (the feed ⋯-menu) that reaches
/// the web-publishing verbs without ever having opened this page. A no-op if
/// already cached or already hydrating. On success, caches the read and calls
/// `on_ready` — the caller's job to trigger a repaint (mirroring
/// `manager.refresh_current_feed()`, which is what the mutations themselves
/// already use to repaint after a nest-confirmed change), so the popover it
/// is about to show — or the next one built — carries the resolved origin
/// instead of a permanently-optimistic guess.
pub(crate) fn ensure_hydrated(
    client: &Rc<crate::client::FaunaClient>,
    on_ready: impl FnOnce() + 'static,
) {
    if CACHED_WEB_PAGE.with(|c| c.borrow().is_some()) {
        return;
    }
    if WEB_PAGE_HYDRATING.with(|f| f.replace(true)) {
        return;
    }
    // The identity seam ahead of the write below (`actor_scope.rs`): captured
    // before the read, re-checked at landing — a stale landing must not clear
    // the INCOMING actor's in-flight latch (which an actor change may have
    // already reset to `false` for a hydrate the incoming actor itself
    // started) or seed its cache with the outgoing actor's origin.
    let generation = crate::actor_scope::current_generation();
    let web = WebRpc::new(client.nest_rpc().clone());
    let (handle, _, _) = crate::client::load_account_cache();
    spawn_with_snapshot(
        &client.runtime_handle(),
        move || async move {
            fauna_client_web::read_web_page(&web, handle.as_deref().unwrap_or("")).await
        },
        move |res| {
            if !crate::actor_scope::poll_still_current(generation) {
                return;
            }
            WEB_PAGE_HYDRATING.with(|f| f.set(false));
            if let Ok(read) = res {
                CACHED_WEB_PAGE.with(|c| *c.borrow_mut() = Some(read));
                on_ready();
            }
        },
    );
}

#[derive(Clone)]
struct WebWidgets {
    toggle: adw::SwitchRow,
    url_row: adw::ActionRow,
    url_marker: gtk::Label,
    render_status: gtk::Label,
    error_label: gtk::Label,
    published_group: adw::PreferencesGroup,
    published_list: gtk::Box,
    published_rows: Rc<RefCell<Vec<gtk::Box>>>,
    published_empty: gtk::Label,
    paywall_note: gtk::Label,
    no_origin_note: gtk::Label,
}

struct WebCtx {
    web: Arc<WebRpc>,
    rt: tokio::runtime::Handle,
    // Suppresses the toggle handler while `render` syncs it programmatically.
    guard: Rc<Cell<bool>>,
    w: WebWidgets,
}

/// Build the **`web-settings`** page. Returns the page widget plus a
/// `refresh` entry point the settings shell wires to its on-visible hook
/// (mirrors `build_linked_nests_page`/`build_subscriptions_page`): the
/// Published-posts list is observer-free, so a re-visit must re-read
/// `publish.list` or a post published from the feed ⋯-menu — or from another
/// device — never appears.
pub fn build_web_page() -> (gtk::Box, Rc<dyn Fn()>) {
    let page = adw::PreferencesPage::builder()
        .title(WS::TITLE)
        .icon_name("network-server-symbolic")
        .build();

    let group = adw::PreferencesGroup::builder().title(WS::TITLE).build();
    page.add(&group);

    // Subdomain opt-in toggle (default OFF).
    let toggle = adw::SwitchRow::builder().active(false).build();
    toggle.set_title(WS::SUBDOMAIN_TOGGLE_LABEL);
    toggle.set_subtitle(WS::SUBDOMAIN_TOGGLE_SUBTITLE);
    set_test_id(&toggle, ids::WEB_SETTINGS_SUBDOMAIN_TOGGLE);
    crate::offline_gate::declare_wire_kind(&toggle, "fauna.web.set_subdomain_enabled");
    group.add(&toggle);

    // Live URL / disabled-reason row. The marker label carries the text the e2e
    // reads (web-settings-subdomain-url).
    let url_row = adw::ActionRow::builder()
        .title(WS::SUBDOMAIN_URL_LABEL)
        .subtitle("")
        .build();
    let url_marker = gtk::Label::builder().visible(false).build();
    set_test_id(&url_marker, ids::WEB_SETTINGS_SUBDOMAIN_URL);
    url_row.add_suffix(&url_marker);
    group.add(&url_row);

    // The blanked-site status line: information only, no gesture — the nest
    // restores the site by itself (`web-content-hosting.md` § Routing, render,
    // serving → *A blanked site tells its author*). Hidden until a read says the
    // rendered pages are down; a hidden label never registers for the e2e.
    let render_status = gtk::Label::builder()
        .label(WS::RENDER_STATUS_DOWN)
        .wrap(true)
        .xalign(0.0)
        .css_classes(["dim-label"])
        .visible(false)
        .build();
    set_test_id(&render_status, ids::WEB_SETTINGS_RENDER_STATUS);
    let render_status_row = adw::ActionRow::builder().activatable(false).build();
    render_status_row.add_suffix(&render_status);
    group.add(&render_status_row);

    // Static content explainer.
    let info_row = adw::ActionRow::builder().activatable(false).build();
    let info_label = gtk::Label::builder()
        .label(WS::CONTENT_INFO)
        .wrap(true)
        .xalign(0.0)
        .css_classes(["dim-label"])
        .build();
    set_test_id(&info_label, ids::WEB_SETTINGS_CONTENT_INFO);
    info_row.add_suffix(&info_label);
    group.add(&info_row);

    // Page-level error label (Rule 2), hidden until set.
    let error_label = gtk::Label::builder().visible(false).build();
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    let error_row = adw::ActionRow::builder().activatable(false).build();
    error_row.add_suffix(&error_label);
    group.add(&error_row);

    // ── Published-posts management section (web-content-hosting.md
    //    § Published-post management) — starts hidden until the page's first
    //    hydrate lands; painting it off unread state would tell a creator with
    //    published posts that they have none.
    let published_group = adw::PreferencesGroup::builder()
        .title(WS::PUBLISHED_POSTS_TITLE)
        .visible(false)
        .build();
    page.add(&published_group);

    let paywall_note = gtk::Label::builder()
        .label(WP::PAYWALL_LINK_NOTE)
        .wrap(true)
        .xalign(0.0)
        .css_classes(["dim-label"])
        .visible(false)
        .build();
    let paywall_note_row = adw::ActionRow::builder().activatable(false).build();
    paywall_note_row.add_suffix(&paywall_note);
    published_group.add(&paywall_note_row);

    let no_origin_note = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .css_classes(["dim-label"])
        .visible(false)
        .build();
    let no_origin_row = adw::ActionRow::builder().activatable(false).build();
    no_origin_row.add_suffix(&no_origin_note);
    published_group.add(&no_origin_row);

    let published_empty = gtk::Label::builder()
        .label(WS::PUBLISHED_POSTS_EMPTY)
        .halign(gtk::Align::Start)
        .css_classes(["dim-label"])
        .visible(false)
        .build();
    set_test_id(&published_empty, ids::WEB_PUBLISHED_POSTS_EMPTY);
    let published_empty_row = adw::ActionRow::builder().activatable(false).build();
    published_empty_row.add_suffix(&published_empty);
    published_group.add(&published_empty_row);

    // The row container. Its own row (not a `ListBox`) so a repaint is a plain
    // remove-then-append of `gtk::Box` rows, the `subscription-mine-row` idiom.
    let published_list = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&published_list, ids::WEB_PUBLISHED_POSTS_LIST);
    let published_list_row = adw::ActionRow::builder().activatable(false).build();
    published_list_row.set_child(Some(&published_list));
    published_group.add(&published_list_row);

    let widgets = WebWidgets {
        toggle,
        url_row,
        url_marker,
        render_status,
        error_label,
        published_group,
        published_list,
        published_rows: Rc::new(RefCell::new(Vec::new())),
        published_empty,
        paywall_note,
        no_origin_note,
    };
    let refresh = wire(widgets);
    (
        crate::testid::wrap_page_with_heading(WS::TITLE, ids::PAGE_HEADING, &page),
        refresh,
    )
}

fn wire(w: WebWidgets) -> Rc<dyn Fn()> {
    let client = match crate::settings::get_client() {
        Some(c) => c,
        // No client (unit test / pre-auth): the page stays at its static
        // placeholders, and a refresh has nothing to re-read.
        None => return Rc::new(|| {}),
    };
    let ctx = Rc::new(WebCtx {
        web: Arc::new(WebClient::new(client.nest_rpc().clone())),
        rt: client.runtime_handle(),
        guard: Rc::new(Cell::new(false)),
        w,
    });

    {
        let ctx = Rc::clone(&ctx);
        let toggle = ctx.w.toggle.clone();
        toggle.connect_active_notify(move |sw| {
            if ctx.guard.get() {
                return; // programmatic sync from render(), not a user action
            }
            set_enabled(&ctx, sw.is_active());
        });
    }

    hydrate(&ctx);
    let ctx_refresh = Rc::clone(&ctx);
    Rc::new(move || hydrate(&ctx_refresh))
}

/// Read the full `web-settings` page (retrying the opt-in flag while the WS
/// comes up after login) and render both surfaces from one answer. Runs both
/// at page construction and on every re-visit (the settings shell's
/// on-visible hook), since the Published-posts list is observer-free.
fn hydrate(ctx: &Rc<WebCtx>) {
    let web = Arc::clone(&ctx.web);
    let ctx_render = Rc::clone(ctx);
    let generation = crate::actor_scope::current_generation();
    let (handle, _, _) = crate::client::load_account_cache();
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            // A single NestClient RPC — the transport already parks it while
            // reconnecting (transport.md § Request lifecycle step 3), so no
            // app-side retry is needed.
            web.get_subdomain_enabled()
                .await
                .map_err(|e| e.to_string())?;
            fauna_client_web::read_web_page(&web, handle.as_deref().unwrap_or("")).await
        },
        move |res| {
            // The identity seam: a read spawned for the outgoing actor must not
            // paint the incoming actor's Settings → Web page or seed its cache
            // (`actor_scope.rs`).
            if !crate::actor_scope::poll_still_current(generation) {
                return;
            }
            if let Ok(read) = &res {
                CACHED_WEB_PAGE.with(|c| *c.borrow_mut() = Some(read.clone()));
            }
            render(&ctx_render, res);
            ctx_render.w.published_group.set_visible(true);
        },
    );
}

/// Flip the opt-in, then re-project from the nest's echoed state — reusing the
/// already-cached domains/serving_domain/posts rather than a second full read
/// (mirrors tui's `Action::ToggleWebSubdomain`).
fn set_enabled(ctx: &Rc<WebCtx>, enabled: bool) {
    let web = Arc::clone(&ctx.web);
    let ctx_render = Rc::clone(ctx);
    let generation = crate::actor_scope::current_generation();
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            web.set_subdomain_enabled(enabled)
                .await
                .map_err(|e| e.to_string())
        },
        move |res| {
            // The identity seam: an echo landing after an actor change must not
            // fold into the incoming actor's cache or repaint their page.
            if !crate::actor_scope::poll_still_current(generation) {
                return;
            }
            let read = match res {
                Ok(echoed) => CACHED_WEB_PAGE.with(|c| {
                    let mut c = c.borrow_mut();
                    let (handle, _, _) = crate::client::load_account_cache();
                    if let Some(cur) = c.as_mut() {
                        cur.view = fauna_client_web::subdomain_view(
                            echoed,
                            handle.as_deref().filter(|h| !h.is_empty()),
                            &cur.serving_domain,
                        );
                        Ok(cur.clone())
                    } else {
                        // Flipping requires the page to already be hydrated —
                        // the toggle is inert until then — so this arm should
                        // be unreachable; degrade honestly rather than guess.
                        Err("web hosting state not loaded yet".to_string())
                    }
                }),
                Err(e) => Err(format!("set web hosting: {e}")),
            };
            render(&ctx_render, read);
        },
    );
}

fn render(ctx: &Rc<WebCtx>, res: Result<WebPageRead, String>) {
    let w = &ctx.w;

    let (snap_enabled, snap_url, snap_disabled_reason, snap_error, posts) = match &res {
        Ok(read) => (
            read.view.enabled,
            read.view.url.clone(),
            read.view.disabled_reason,
            None,
            Some(read.posts.clone()),
        ),
        Err(e) => (false, None, None, Some(e.clone()), None),
    };

    // Sync the toggle without re-triggering the handler.
    ctx.guard.set(true);
    w.toggle.set_active(snap_enabled);
    ctx.guard.set(false);
    // Carry the toggle's on/off for the e2e driver via the `state` attr (the
    // uniform read idiom: driver.get_attr(id, "state")). Non-optimistic — set
    // only from a nest-confirmed snapshot, so it doubles as the round-trip proof.
    set_test_attr(&w.toggle, "state", if snap_enabled { "on" } else { "off" });

    // URL row: the live URL (whether on or off — it's where the site would
    // serve), else the disabled reason.
    let url_text = match (&snap_url, snap_disabled_reason) {
        (Some(url), _) => url.clone(),
        (None, Some(SubdomainDisabledReason::NoHandle)) => WS::SUBDOMAIN_NO_HANDLE.to_string(),
        (None, Some(SubdomainDisabledReason::ReservedLabel)) => WS::SUBDOMAIN_RESERVED.to_string(),
        // The nest serves no web content at any host — say so, rather than
        // leaving the row blank (`web-content-hosting.md` § Published-post
        // management: legal but unreachable, and the UI must say so).
        (None, Some(SubdomainDisabledReason::NoServingDomain)) => {
            WS::SUBDOMAIN_NO_SERVING_DOMAIN.to_string()
        }
        (None, None) => String::new(),
    };
    w.url_row.set_subtitle(&url_text);
    w.url_marker.set_text(&url_text);
    w.url_marker.set_visible(!url_text.is_empty());

    // Painted only off a nest-confirmed read that says the pages are down; an
    // errored or not-yet-read page paints nothing.
    w.render_status.set_visible(
        res.as_ref()
            .map(|read| read.rendered_pages_down)
            .unwrap_or(false),
    );

    super::render_error_label(&w.error_label, snap_error.as_deref());

    if let Some(posts) = posts {
        render_published_posts(ctx, &posts);
    }
}

/// The origin every copy affordance builds on — active custom domain beats an
/// enabled subdomain (`fauna_client_web::site_link_view`), resolved from the
/// SAME cached read the section renders from, so the toggle above and the
/// links below can never disagree.
fn resolve_site_link(read: &WebPageRead) -> fauna_client_web::SiteLinkView {
    let (handle, _, _) = crate::client::load_account_cache();
    fauna_client_web::site_link_view(
        &read.domains,
        read.view.enabled,
        handle.as_deref().filter(|h| !h.is_empty()),
        &read.serving_domain,
    )
}

/// The shared [`fauna_client_web::disabled_reason_text`] decision (tui↔linux
/// twin harvest, previously hand-rolled identically
/// here and on tui).
fn disabled_reason_text(reason: Option<SiteLinkDisabledReason>) -> String {
    fauna_client_web::disabled_reason_text(reason).resolve(crate::i18n::strings::lookup)
}

/// Rebuild the Published-posts rows from a fresh `publish.list` read. Always
/// re-derives the origin from the cache rather than trusting a stale one, so a
/// toggle flip that landed between two reads never leaves a row advertising a
/// dead link.
fn render_published_posts(ctx: &Rc<WebCtx>, posts: &[PublishedPost]) {
    let w = &ctx.w;
    let link = cached_site_link();
    let origin = link.as_ref().and_then(|l| l.origin.as_deref());
    let any_gated = posts.iter().any(|p| p.gated_tier.is_some());

    w.paywall_note.set_visible(any_gated);
    w.no_origin_note
        .set_visible(origin.is_none() && !posts.is_empty());
    if let Some(link) = &link {
        w.no_origin_note
            .set_text(&disabled_reason_text(link.disabled_reason));
    }

    w.published_empty.set_visible(posts.is_empty());

    {
        let mut rows = w.published_rows.borrow_mut();
        for row in rows.drain(..) {
            w.published_list.remove(&row);
        }
    }
    for (i, post) in posts.iter().enumerate() {
        let row = build_published_row(ctx, post, i, origin);
        w.published_list.append(&row);
        w.published_rows.borrow_mut().push(row);
    }
}

fn build_published_row(
    ctx: &Rc<WebCtx>,
    post: &PublishedPost,
    index: usize,
    origin: Option<&str>,
) -> gtk::Box {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    set_test_id(&row, ids::WEB_PUBLISHED_POST_ITEM);
    set_test_attr(&row, "gated-tier", post.gated_tier.as_deref().unwrap_or(""));

    let slug = gtk::Label::new(Some(&post.slug));
    slug.set_halign(gtk::Align::Start);
    slug.set_hexpand(true);
    slug.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    set_test_id(&slug, ids::WEB_PUBLISHED_POST_SLUG);
    row.append(&slug);

    if let Some(tier) = &post.gated_tier {
        let badge = gtk::Label::new(Some(&WS::published_post_gated_badge(tier)));
        badge.add_css_class("dim-label");
        row.append(&badge);
    }

    let copy_link_btn = gtk::Button::with_label(WP::COPY_WEB_LINK);
    set_test_id(&copy_link_btn, ids::WEB_PUBLISHED_POST_COPY_LINK_BUTTON);
    copy_link_btn.set_sensitive(origin.is_some());
    if let Some(origin) = origin {
        let url = fauna_client_web::post_page_url(origin, &post.slug);
        copy_link_btn.connect_clicked(move |btn| {
            crate::clipboard::copy_text(&url);
            set_test_attr(btn, "copied", &url);
        });
    }
    row.append(&copy_link_btn);

    // Gated rows only: an ungated post has no paywalled body to hand out, so
    // the affordance would mint a token for nothing.
    if post.gated_tier.is_some() {
        let paywall_btn = gtk::Button::with_label(WP::COPY_PAYWALL_LINK);
        set_test_id(
            &paywall_btn,
            ids::WEB_PUBLISHED_POST_COPY_PAYWALL_LINK_BUTTON,
        );
        crate::offline_gate::declare_wire_kind(&paywall_btn, "fauna.web.paywall.mint_token");
        paywall_btn.set_sensitive(origin.is_some());
        if let Some(origin) = origin {
            let ctx = Rc::clone(ctx);
            let slug = post.slug.clone();
            let origin = origin.to_string();
            paywall_btn.connect_clicked(move |btn| {
                mint_paywall_link(&ctx, slug.clone(), origin.clone(), btn.clone());
            });
        }
        row.append(&paywall_btn);
    }

    let unpublish_btn = gtk::Button::with_label(WP::UNPUBLISH);
    unpublish_btn.add_css_class("destructive-action");
    set_test_id(&unpublish_btn, ids::WEB_PUBLISHED_POST_UNPUBLISH_BUTTON);
    crate::offline_gate::declare_wire_kind(&unpublish_btn, "fauna.web.publish.unset");
    {
        let ctx = Rc::clone(ctx);
        let post_id = post.post_id.clone().into_vec();
        unpublish_btn.connect_clicked(move |_| unpublish(&ctx, post_id.clone()));
    }
    row.append(&unpublish_btn);

    let _ = index; // rows are addressed by AT-SPI tree position, not a stored index
    row
}

/// `fauna.web.paywall.mint_token` for a gated, published own post — a fresh
/// mint per click, since the token is short-lived by ratified design and
/// re-minting is free (`monetization.md` § Pillar 2 → Creator comp-link
/// surface).
fn mint_paywall_link(ctx: &Rc<WebCtx>, slug: String, origin: String, btn: gtk::Button) {
    let web = Arc::clone(&ctx.web);
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            web.paywall_mint_token(fauna_client_web::PaywallTarget::PostSlug { slug })
                .await
                .map(|minted| fauna_client_web::tokened_url(&origin, &minted.path, &minted.token))
                .map_err(|e| format!("mint paywall link: {e}"))
        },
        move |res| match res {
            Ok(url) => {
                crate::clipboard::copy_text(&url);
                set_test_attr(&btn, "copied", &url);
                clear_error(&ctx_render.w);
            }
            Err(msg) => show_error(&ctx_render.w, &msg),
        },
    );
}

/// Take a published post down, then re-read `publish.list` so the section
/// reflects the nest's state — a takedown that half-applied then shows up as a
/// row that stayed rather than one that vanished from a screen the nest
/// disagrees with (mirrors tui's `Op::WebUnpublish`).
fn unpublish(ctx: &Rc<WebCtx>, post_id: Vec<u8>) {
    let web = Arc::clone(&ctx.web);
    let ctx_render = Rc::clone(ctx);
    let generation = crate::actor_scope::current_generation();
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            web.publish_unset(post_id)
                .await
                .map_err(|e| format!("unpublish post: {e}"))?;
            web.publish_list()
                .await
                .map_err(|e| format!("reload published posts: {e}"))
        },
        move |res| {
            // The identity seam: a takedown confirmation landing after an actor
            // change must not fold its posts into the incoming actor's cache.
            if !crate::actor_scope::poll_still_current(generation) {
                return;
            }
            match res {
                Ok(site) => {
                    clear_error(&ctx_render.w);
                    // The whole answer, not just the rows: a takedown can be
                    // what restores (or blanks) the rendered pages, and the
                    // status line must follow the same read.
                    CACHED_WEB_PAGE.with(|c| {
                        if let Some(cur) = c.borrow_mut().as_mut() {
                            cur.posts = site.posts.clone();
                            cur.rendered_pages_down = site.rendered_pages_down;
                        }
                    });
                    ctx_render
                        .w
                        .render_status
                        .set_visible(site.rendered_pages_down);
                    render_published_posts(&ctx_render, &site.posts);
                }
                Err(msg) => show_error(&ctx_render.w, &msg),
            }
        },
    );
}

fn show_error(w: &WebWidgets, msg: &str) {
    super::render_error_label(&w.error_label, Some(msg));
}

fn clear_error(w: &WebWidgets) {
    super::render_error_label(&w.error_label, None);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_builds_without_client() {
        crate::testid::run_on_gtk_thread(|| {
            let (_page, _refresh) = build_web_page();
        });
    }

    /// **The canonical actor-scoped drop clears both the cache AND the
    /// in-flight latch** (`account-scoping.md` § The scoping taxonomy → the
    /// in-memory corollary) — an
    /// account switch must not let the incoming actor's copy-link verbs
    /// resolve against the outgoing actor's cached origin, and must not leave
    /// a stuck `WEB_PAGE_HYDRATING` latch that starves the incoming actor's
    /// own first hydrate.
    ///
    /// Red-verify by dropping either line from [`clear_for_actor_change`]:
    /// the corresponding assertion fails.
    #[test]
    fn clear_for_actor_change_drops_the_cache_and_the_hydrating_latch() {
        let sample = WebPageRead {
            view: fauna_client_web::SubdomainView {
                enabled: true,
                url: Some("https://example.test/".to_string()),
                disabled_reason: None,
            },
            serving_domain: "example.test".to_string(),
            domains: Vec::new(),
            posts: Vec::new(),
            rendered_pages_down: false,
        };
        CACHED_WEB_PAGE.with(|c| *c.borrow_mut() = Some(sample));
        WEB_PAGE_HYDRATING.with(|f| f.set(true));

        clear_for_actor_change();

        assert!(
            CACHED_WEB_PAGE.with(|c| c.borrow().is_none()),
            "an account switch must drop the outgoing actor's cached origin"
        );
        assert!(
            !WEB_PAGE_HYDRATING.with(|f| f.get()),
            "an account switch must not leave the latch stuck for the incoming actor"
        );
    }
}
