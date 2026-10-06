//! The Settings → **Connected apps** sub-page (`docs/goal/ui/connected-apps.md`;
//! ui.yaml page `connected-apps`, reached `{"view":"settings","id":"connected-apps"}`).
//! The rail slot is directly after Task delegation (`settings.md` § Navigation
//! model). tui is the lead app (`apps/fauna-tui/src/settings/connected_apps.rs`);
//! this is the same page, region for region.
//!
//! Four regions, top to bottom: **Requests** (the quiet-push tray — one built
//! consent card per live request, painted only while a request is live, never
//! a "no requests" row), **Connect an app** (the typed-code start), **the
//! roster** (one row per connected app, Revoke with an inline confirm; a mail
//! app password adds its login, kind and secret controls) and **Blocked apps**
//! (painted only while something is blocked).
//!
//! **A paint shell over the shared `ConnectedAppsMachine`**
//! (`libs/fauna-client-connected-apps`): the roster's composition, the scope
//! words, the class badge key, *lasts-until* and which verb revokes a row are
//! the machine's. This file never picks a revoke verb — a row's `key` is
//! opaque here. The consent card is the built card
//! (`fauna_atproto_settings_machine::consent_card_row`'s composition), never a
//! second one.
//!
//! **The lift.** The AT Protocol page's consent cards and connected-app rows,
//! the Nostr page's bunker rows and the Mail & Calendar page's app-password
//! rows render HERE and no longer on their old pages — a row moves, it is never
//! shown twice (`connected-apps.md` § Architectural rules).
//!
//! **A visit starts unread.** Rows are nest state read on every open, so the
//! page drops the previous visit's rows (and every shown secret, and an armed
//! revoke) and paints neither rows nor the empty state until its own read has
//! returned. The page re-reads each time it is mapped; the machine itself is
//! built on the first visit, once the session's mail-settings machine (whose
//! app passwords are rows of this roster) exists.

use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;

use fauna_client_connected_apps::{
    BlockedAppRow, ConnectedAppRow, ConnectedAppsMachine, ConnectedAppsObserver,
    ConnectedAppsSnapshot, ConsentCardRow, class,
};
use fauna_client_mail_settings::resolve_mua_username;
use fauna_core::localized::LocalizedText;
use fauna_core::secret::SecretString;

use crate::async_helper::spawn_with_snapshot;
use crate::i18n::strings::settings::mail as mail_t;
use crate::i18n::strings::{atproto_settings as card_t, connected_apps as t};
use crate::testid::{set_test_attr, set_test_id, set_test_text};

/// The rail label + `add_titled` title (settings_shell.rs).
pub const TITLE: &str = t::TITLE;

/// This page repaints from a fresh `snapshot()` after every awaited gesture,
/// so the machine's own change callback has nothing to do.
struct NoopObserver;

impl ConnectedAppsObserver for NoopObserver {
    fn on_changed(&self) {}
}

/// One gesture on the page, carried across the spawn.
#[derive(Clone)]
enum Gesture {
    Refresh,
    SubmitCode(String),
    Resolve {
        consent_id_hex: String,
        approved: bool,
    },
    Block(String),
    Unblock(String),
    Revoke(String),
}

impl Gesture {
    async fn run(self, machine: &ConnectedAppsMachine) {
        match self {
            Self::Refresh => machine.refresh().await,
            Self::SubmitCode(code) => machine.submit_code(code).await,
            Self::Resolve {
                consent_id_hex,
                approved,
            } => machine.resolve_request(consent_id_hex, approved).await,
            Self::Block(consent_id_hex) => machine.block_request(consent_id_hex).await,
            Self::Unblock(client_id) => machine.unblock(client_id).await,
            Self::Revoke(key) => machine.revoke(key).await,
        }
    }
}

/// Widget handles the render + event closures need.
struct Widgets {
    error_label: gtk::Label,
    requests_group: adw::PreferencesGroup,
    requests_box: gtk::Box,
    code_entry: adw::EntryRow,
    connect_btn: gtk::Button,
    roster_box: gtk::Box,
    blocked_group: adw::PreferencesGroup,
    blocked_box: gtk::Box,
}

/// Everything the handlers + render need, `Rc`-shared into closures.
struct Ctx {
    rt: tokio::runtime::Handle,
    /// Built on the first visit — see the module docs.
    machine: RefCell<Option<Arc<ConnectedAppsMachine>>>,
    /// The last snapshot painted; `None` until this visit's read returns.
    snapshot: RefCell<Option<ConnectedAppsSnapshot>>,
    /// The row key whose inline revoke confirm is open.
    revoke_armed: RefCell<Option<String>>,
    /// The mail app-password secrets currently shown, by row key. Empty by
    /// default: the secret is never in the snapshot, so a row shows one only
    /// after the user asks and the on-demand read resolves. Keyed by row key,
    /// never by index, so a roster that re-orders under a fresh snapshot cannot
    /// show one password's secret against another row.
    revealed: RefCell<HashMap<String, SecretString>>,
    w: Widgets,
}

fn resolve(text: &LocalizedText) -> String {
    text.clone().resolve(crate::i18n::strings::lookup)
}

fn when(ms: i64) -> String {
    fauna_core::format::format_unix_local_ms(ms)
}

/// The class badge's words — the one per-app half of the grouping key, which
/// the machine derives. An unknown class paints no badge rather than a guess.
fn class_label(class: &str) -> Option<&'static str> {
    Some(match class {
        class::REMOTE => t::CLASS_REMOTE,
        class::DEVICE => t::CLASS_DEVICE,
        class::WASM => t::CLASS_WASM,
        class::CONTAINER => t::CLASS_CONTAINER,
        class::APP_PASSWORD => t::CLASS_APP_PASSWORD,
        class::SIGNER => t::CLASS_SIGNER,
        class::OAUTH => t::CLASS_OAUTH,
        _ => return None,
    })
}

/// Build the "Connected apps" preferences page. Constructs every static ui.yaml
/// ID even when no client is registered (the clientless unit test); only the
/// async wiring is gated on a live client.
pub fn build_connected_apps_page() -> (adw::PreferencesPage, impl Fn() + 'static) {
    let page = adw::PreferencesPage::builder()
        .title(t::TITLE)
        .icon_name("network-wired-symbolic")
        .build();

    // --- Top group: heading + description + page-level error ---
    let top_group = adw::PreferencesGroup::builder()
        .title(t::TITLE)
        .description(t::DESCRIPTION)
        .build();
    top_group.set_header_suffix(Some(&super::marker("page-heading")));
    let error_label = gtk::Label::builder().visible(false).build();
    error_label.add_css_class("error");
    error_label.set_halign(gtk::Align::Start);
    error_label.set_wrap(true);
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    top_group.add(&error_label);
    page.add(&top_group);

    // --- 1. Requests — only while a request is live ---
    let requests_group = adw::PreferencesGroup::builder()
        .title(t::REQUESTS_HEADING)
        .visible(false)
        .build();
    let requests_box = vertical_box();
    requests_group.add(&requests_box);
    page.add(&requests_group);

    // --- 2. Connect an app ---
    let connect_group = adw::PreferencesGroup::builder()
        .title(t::CONNECT_HEADING)
        .description(t::CONNECT_HINT)
        .build();
    let code_entry = adw::EntryRow::builder()
        .title(t::CONNECT_PLACEHOLDER)
        .build();
    set_test_id(&code_entry, ids::CONNECTED_APPS_CONNECT_CODE);
    let connect_btn = gtk::Button::builder()
        .label(t::CONNECT_SUBMIT)
        .valign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .sensitive(false)
        .build();
    set_test_id(&connect_btn, ids::CONNECTED_APPS_CONNECT_SUBMIT);
    crate::offline_gate::declare_wire_kind(&connect_btn, "fauna.oauth.consent.lookup_code");
    code_entry.add_suffix(&connect_btn);
    connect_group.add(&code_entry);
    page.add(&connect_group);

    // --- 3. The roster ---
    let roster_group = adw::PreferencesGroup::builder()
        .title(t::ROSTER_HEADING)
        .build();
    let roster_box = vertical_box();
    roster_group.add(&roster_box);
    page.add(&roster_group);

    // --- 4. Blocked apps — only while something is blocked ---
    let blocked_group = adw::PreferencesGroup::builder()
        .title(t::BLOCKED_HEADING)
        .description(t::BLOCKED_HINT)
        .visible(false)
        .build();
    let blocked_box = vertical_box();
    blocked_group.add(&blocked_box);
    page.add(&blocked_group);

    let widgets = Widgets {
        error_label,
        requests_group,
        requests_box,
        code_entry,
        connect_btn,
        roster_box,
        blocked_group,
        blocked_box,
    };
    // Every visit is a re-read: rows are nest state, and a quiet push raises no
    // event the page could wait on. The settings shell calls the returned
    // closure from its visible-child hook, which fires both on arriving and on
    // re-selecting the page that is already showing (a re-selection never
    // re-maps — `nav_rail::set_visible_child_forced`).
    let visit = wire(widgets);
    let refresh = move || {
        if let Some(visit) = &visit {
            visit();
        }
    };
    (page, refresh)
}

fn vertical_box() -> gtk::Box {
    gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .accessible_role(gtk::AccessibleRole::Group)
        .build()
}

/// Wire the page: the code field's submit, and return the closure each visit
/// (and each consent push) runs. `None` (the page stays static) when no client
/// is registered — e.g. the unit test.
fn wire(widgets: Widgets) -> Option<impl Fn()> {
    let client = crate::settings::get_client()?;
    let ctx = Rc::new(Ctx {
        rt: client.runtime_handle(),
        machine: RefCell::new(None),
        snapshot: RefCell::new(None),
        revoke_armed: RefCell::new(None),
        revealed: RefCell::new(HashMap::new()),
        w: widgets,
    });

    {
        let ctx_changed = Rc::clone(&ctx);
        ctx.w.code_entry.connect_changed(move |entry| {
            ctx_changed
                .w
                .connect_btn
                .set_sensitive(!entry.text().trim().is_empty());
        });
    }
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.connect_btn.clone().connect_clicked(move |_| {
            submit_code(&ctx);
        });
    }
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.code_entry.clone().connect_apply(move |_| {
            submit_code(&ctx);
        });
    }

    // A consent push re-reads the page while it is showing: the request is one
    // the user must answer within minutes (the same nudge the AT Protocol page
    // took before the card moved here).
    {
        let ctx = Rc::clone(&ctx);
        crate::settings::set_connected_apps_rehydrate_handler(Rc::new(move || {
            if ctx.snapshot.borrow().is_some() {
                run(&ctx, Gesture::Refresh);
            }
        }));
    }

    Some(move || visit(&ctx))
}

/// A fresh visit: drop the last visit's state, paint unread, re-read.
fn visit(ctx: &Rc<Ctx>) {
    if ctx.machine.borrow().is_none() {
        // The session's mail machine is built by the Mail & Calendar page;
        // its app passwords are rows of this roster.
        let Some(client) = crate::settings::get_client() else {
            return;
        };
        let observer: Arc<dyn ConnectedAppsObserver> = Arc::new(NoopObserver);
        let machine = fauna_client_connected_apps::build_connected_apps_machine(
            client.nest_rpc().clone(),
            observer,
            crate::settings::mail_machine(),
        );
        // An approve of a records consent mints the app's grant; an
        // undecodable secret leaves it unwired, which refuses that approve.
        match fauna_core::identity::ActorKeypair::from_secret_hex(client.secret_hex()) {
            Ok(keypair) => {
                machine.set_consent_grant_seams(crate::mail_glue::consent_grant_seams(&keypair))
            }
            Err(e) => tracing::error!("[settings/connected-apps] decode secret_hex: {e}"),
        }
        *ctx.machine.borrow_mut() = Some(machine);
    }
    ctx.w.code_entry.set_text("");
    ctx.revoke_armed.borrow_mut().take();
    ctx.revealed.borrow_mut().clear();
    ctx.snapshot.borrow_mut().take();
    paint(ctx);
    run(ctx, Gesture::Refresh);
}

fn submit_code(ctx: &Rc<Ctx>) {
    let code = ctx.w.code_entry.text().trim().to_string();
    if code.is_empty() {
        return;
    }
    ctx.w.code_entry.set_text("");
    run(ctx, Gesture::SubmitCode(code));
}

/// Run one gesture against the machine, then repaint from the fresh snapshot.
fn run(ctx: &Rc<Ctx>, gesture: Gesture) {
    let Some(machine) = ctx.machine.borrow().clone() else {
        return;
    };
    let ctx = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt.clone(),
        move || async move {
            gesture.run(&machine).await;
            machine.snapshot()
        },
        move |snapshot| {
            *ctx.snapshot.borrow_mut() = Some(snapshot);
            paint(&ctx);
        },
    );
}

/// Repaint every region from the stored snapshot (GTK main thread).
fn paint(ctx: &Rc<Ctx>) {
    let w = &ctx.w;
    let snapshot = ctx.snapshot.borrow();
    let snap = snapshot.as_ref();

    super::render_error_label(
        &w.error_label,
        snap.and_then(|s| s.error.as_ref()).map(resolve).as_deref(),
    );

    clear(&w.requests_box);
    let requests: &[ConsentCardRow] = snap.map(|s| s.requests.as_slice()).unwrap_or(&[]);
    w.requests_group.set_visible(!requests.is_empty());
    for request in requests {
        w.requests_box.append(&request_card(ctx, request));
    }

    // The three-state list: nothing until the roster read has returned, then
    // either rows or the empty state (`ui/README.md` § List pages).
    clear(&w.roster_box);
    if let Some(snap) = snap.filter(|s| s.loaded) {
        if snap.principals.is_empty() {
            let empty = gtk::Label::new(Some(t::EMPTY));
            empty.set_halign(gtk::Align::Start);
            empty.add_css_class("dim-label");
            set_test_id(&empty, ids::CONNECTED_APPS_EMPTY);
            w.roster_box.append(&empty);
        }
        let handle = crate::client::load_account_cache().0.unwrap_or_default();
        for row in &snap.principals {
            w.roster_box.append(&roster_row(ctx, row, &handle));
        }
    }

    clear(&w.blocked_box);
    let blocked: &[BlockedAppRow] = snap.map(|s| s.blocked.as_slice()).unwrap_or(&[]);
    w.blocked_group.set_visible(!blocked.is_empty());
    for b in blocked {
        w.blocked_box.append(&blocked_row(ctx, b));
    }
}

fn clear(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

fn card_box() -> gtk::Box {
    let item = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    item.add_css_class("card");
    item.set_margin_top(4);
    item.set_margin_bottom(4);
    item
}

fn text_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_halign(gtk::Align::Start);
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_selectable(false);
    label
}

fn flat_button(label: &str, id: &str) -> gtk::Button {
    let btn = gtk::Button::builder()
        .label(label)
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    set_test_id(&btn, id);
    btn
}

fn leaf(id: &str, text: &str) -> gtk::Label {
    let label = text_label(text);
    set_test_id(&label, id);
    label
}

/// One roster row: the joined description as the item's own text (ui.yaml
/// mints no per-field leaves for the columns every row has), a mail app
/// password's own leaves, then Revoke, or the armed confirm pair.
fn roster_row(ctx: &Rc<Ctx>, row: &ConnectedAppRow, handle: &str) -> gtk::Box {
    let item = card_box();
    set_test_id(&item, ids::CONNECTED_APPS_ITEM);

    let name = resolve(&row.name);
    let mut head = name.clone();
    if let Some(badge) = class_label(&row.class) {
        head = format!("{head} · {badge}");
    }
    let mut lines = vec![head.clone()];
    let burned = row.mail.as_ref().is_some_and(|m| m.revoked);
    // The burned state sits directly under the name and above everything a
    // user might copy into a mail app: the login below it authenticates
    // nothing any more (`mail-credentials.md` § Rotation and recovery →
    // *Succession*).
    if burned {
        lines.push(mail_t::CREDENTIAL_REVOKED.to_string());
    }
    if let (Some(client_id), Some(domain)) = (&row.client_id, &row.publisher) {
        lines.push(format!("{} — {client_id}", t::publisher(domain)));
    }
    for scope in &row.scope_descriptions {
        lines.push(format!("  • {}", resolve(scope)));
    }
    let mut facts = Vec::new();
    if !row.connected {
        facts.push(t::NOT_CONNECTED.to_string());
    }
    facts.push(t::created(&when(row.created_at_millis)));
    facts.push(match row.last_used_at_millis {
        Some(ms) => t::last_used(&when(ms)),
        None => t::NEVER_USED.to_string(),
    });
    facts.push(match row.lasts_until_millis {
        Some(ms) => t::lasts_until(&when(ms)),
        None => t::OPEN_ENDED.to_string(),
    });
    lines.push(facts.join(" · "));
    // The item's declared text is the joined description, as tui's single
    // label is: the cross-app observable is "it names the app AND what it may
    // reach".
    set_test_text(&item, &lines.join("\n"));
    if burned {
        // The burned state as a value a reader can count, beside the words.
        set_test_attr(&item, "revoked", "true");
    }

    let heading = text_label(&head);
    heading.add_css_class("heading");
    item.append(&heading);
    for line in &lines[1..] {
        let label = text_label(line);
        label.add_css_class("caption");
        item.append(&label);
    }

    if let Some(mail) = &row.mail {
        item.append(&leaf(ids::CONNECTED_APPS_ITEM_TYPE, &resolve(&mail.kind)));
        // The concrete login: shared Rust resolved everything but `{handle}`,
        // so the paint is one substitution and never a locally built address.
        let username = resolve_mua_username(&mail.mua_username, handle);
        item.append(&leaf(ids::CONNECTED_APPS_ITEM_USERNAME, &username));
        let copy_username = flat_button(
            mail_t::COPY_USERNAME,
            ids::CONNECTED_APPS_ITEM_COPY_USERNAME,
        );
        copy_username.connect_clicked(move |_| crate::clipboard::copy_text(&username));
        item.append(&copy_username);

        // ⚠ The hidden secret's text MUST stay EMPTY. The cross-app driver
        // polls it until it turns non-empty and returns that AS the secret, so
        // a mask ("••••") would pass the reveal test without the on-demand
        // read ever running.
        let revealed = ctx.revealed.borrow().get(&row.key).cloned();
        let secret = leaf(
            ids::CONNECTED_APPS_ITEM_SECRET,
            revealed.as_ref().map(|s| s.as_str()).unwrap_or(""),
        );
        secret.set_selectable(revealed.is_some());
        item.append(&secret);

        let reveal = flat_button(
            if revealed.is_some() {
                mail_t::HIDE_SECRET
            } else {
                mail_t::REVEAL_SECRET
            },
            ids::CONNECTED_APPS_ITEM_REVEAL_SECRET,
        );
        {
            let ctx = Rc::clone(ctx);
            let key = row.key.clone();
            let shown = revealed.is_some();
            reveal.connect_clicked(move |_| {
                if shown {
                    ctx.revealed.borrow_mut().remove(&key);
                    paint(&ctx);
                } else {
                    reveal_secret(&ctx, key.clone(), false);
                }
            });
        }
        item.append(&reveal);

        // Copy is independent of the reveal toggle: the secret reaches the
        // clipboard without being painted on a screen someone else can read.
        let copy_secret = flat_button(mail_t::COPY_SECRET, ids::CONNECTED_APPS_ITEM_COPY_SECRET);
        {
            let ctx = Rc::clone(ctx);
            let key = row.key.clone();
            copy_secret.connect_clicked(move |_| reveal_secret(&ctx, key.clone(), true));
        }
        item.append(&copy_secret);
    }

    if ctx.revoke_armed.borrow().as_deref() == Some(row.key.as_str()) {
        item.append(&text_label(&t::revoke_prompt(&name)));
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let confirm = gtk::Button::builder()
            .label(t::REVOKE_CONFIRM)
            .css_classes(["destructive-action"])
            .build();
        set_test_id(&confirm, ids::CONNECTED_APPS_ITEM_REVOKE_CONFIRM);
        {
            let ctx = Rc::clone(ctx);
            let key = row.key.clone();
            confirm.connect_clicked(move |_| {
                ctx.revoke_armed.borrow_mut().take();
                run(&ctx, Gesture::Revoke(key.clone()));
            });
        }
        actions.append(&confirm);
        let cancel = flat_button(t::REVOKE_CANCEL, ids::CONNECTED_APPS_ITEM_REVOKE_CANCEL);
        {
            let ctx = Rc::clone(ctx);
            cancel.connect_clicked(move |_| {
                ctx.revoke_armed.borrow_mut().take();
                paint(&ctx);
            });
        }
        actions.append(&cancel);
        item.append(&actions);
    } else {
        // The verb behind this button is the machine's choice per row
        // (`fauna.principals.revoke`, a bunker revoke, or a mail-credential
        // write), so no single wire kind is declared on it.
        let revoke = gtk::Button::builder()
            .label(t::REVOKE)
            .halign(gtk::Align::End)
            .css_classes(["destructive-action"])
            .build();
        set_test_id(&revoke, ids::CONNECTED_APPS_ITEM_REVOKE);
        {
            let ctx = Rc::clone(ctx);
            let key = row.key.clone();
            revoke.connect_clicked(move |_| {
                *ctx.revoke_armed.borrow_mut() = Some(key.clone());
                paint(&ctx);
            });
        }
        item.append(&revoke);
    }
    item
}

/// Read a mail app password's secret on demand — never in the snapshot — then
/// either show it on the row or copy it to the clipboard.
fn reveal_secret(ctx: &Rc<Ctx>, key: String, copy: bool) {
    let Some(machine) = ctx.machine.borrow().clone() else {
        return;
    };
    let ctx = Rc::clone(ctx);
    let read_key = key.clone();
    spawn_with_snapshot(
        &ctx.rt.clone(),
        move || async move {
            let secret = machine.reveal_secret(read_key).await;
            (secret, machine.snapshot())
        },
        move |(secret, snapshot)| {
            // A failed read has set the machine's page error: repaint to show it.
            *ctx.snapshot.borrow_mut() = Some(snapshot);
            if let Some(secret) = secret {
                if copy {
                    crate::clipboard::copy_text(secret.as_str());
                } else {
                    ctx.revealed.borrow_mut().insert(key, secret);
                }
            }
            paint(&ctx);
        },
    );
}

fn blocked_row(ctx: &Rc<Ctx>, blocked: &BlockedAppRow) -> gtk::Box {
    let item = card_box();
    set_test_id(&item, ids::CONNECTED_APPS_BLOCKED_ITEM);
    // The client id verbatim, as the request card showed it — nothing here
    // parses it into a host or a name.
    let text = format!(
        "{}\n{}",
        blocked.client_id,
        t::blocked_since(&when(blocked.blocked_at_millis))
    );
    set_test_text(&item, &text);
    item.append(&text_label(&text));
    let unblock = gtk::Button::builder()
        .label(t::UNBLOCK)
        .halign(gtk::Align::End)
        .build();
    set_test_id(&unblock, ids::CONNECTED_APPS_BLOCKED_ITEM_UNBLOCK);
    crate::offline_gate::declare_wire_kind(&unblock, "fauna.oauth.consent.block_client");
    {
        let ctx = Rc::clone(ctx);
        let client_id = blocked.client_id.clone();
        unblock.connect_clicked(move |_| run(&ctx, Gesture::Unblock(client_id.clone())));
    }
    item.append(&unblock);
    item
}

/// The consent card's permission-set section as flat lines — one heading per
/// set (title + NSID, or the NSID alone when the publisher declared none), then
/// its `details` when present, then one bulleted line per member. Empty for a
/// request naming no set. Split out so the composition is pinnable without a
/// GTK widget.
fn consent_set_lines(consent: &ConsentCardRow) -> Vec<String> {
    let mut lines = Vec::new();
    for set in &consent.sets {
        lines.push(match set.title.as_deref() {
            Some(title) => card_t::consent_set_heading(title, &set.nsid),
            None => card_t::consent_set_heading_unnamed(&set.nsid),
        });
        if let Some(details) = set.details.as_deref() {
            lines.push(format!("  {details}"));
        }
        for member in &set.member_descriptions {
            lines.push(format!("  • {member}"));
        }
    }
    lines
}

/// One `connected-apps-request-card`: a third-party app is asking to act for
/// this account and is waiting for the answer — the built consent card, with
/// its wording (`atproto_settings.consent_*`) unchanged. Every request the
/// nest lists is painted, an unhinted browser request included; the binding
/// code is the user's check.
///
/// Three things render, and each is load-bearing: **who is asking** (the
/// resolved `client_name` plus the `client_id` verbatim — deliberately no logo:
/// `logo_uri` never crosses to the app at all), **what it is asking for** (one
/// line per scope, worded by the shared `authz::describe_scope` — never a
/// second wording), and **the binding code** (`connected-apps-request-code`),
/// minted by the nest so the value here and the one in the browser have one
/// origin.
///
/// ⚠ Approve and Decline render unconditionally — never gated on how close the
/// request is to expiring. A resolution is reported even past `expires_at`, so
/// "this request just expired" may only ever come from the resolve reply.
fn request_card(ctx: &Rc<Ctx>, consent: &ConsentCardRow) -> gtk::Box {
    let card = card_box();
    set_test_id(&card, ids::CONNECTED_APPS_REQUEST_CARD);

    let who = match &consent.client_name {
        Some(name) => card_t::consent_client(name, &consent.client_id),
        None => card_t::consent_client_unnamed(&consent.client_id),
    };
    let asks: Vec<String> = consent
        .scope_descriptions
        .iter()
        .map(|line| format!("  • {line}"))
        .collect();
    let mut lines = vec![
        card_t::CONSENT_HEADING.to_string(),
        who,
        card_t::CONSENT_SCOPES_HEADING.to_string(),
    ];
    lines.extend(asks);
    lines.extend(consent_set_lines(consent));
    set_test_text(&card, &lines.join("\n"));

    let heading = text_label(&lines[0]);
    heading.add_css_class("heading");
    card.append(&heading);
    for line in &lines[1..] {
        card.append(&text_label(line));
    }

    // The code carries the raw value in a `code` attr as well as inside its
    // prose: the label is localized and may be reworded; the value is not, and
    // it is the value the user compares against their browser.
    let code = leaf(
        ids::CONNECTED_APPS_REQUEST_CODE,
        &card_t::consent_code(&consent.code),
    );
    set_test_attr(&code, "code", &consent.code);
    card.append(&code);
    let hint = text_label(card_t::CONSENT_CODE_HINT);
    hint.add_css_class("dim-label");
    card.append(&hint);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    for (id, label, approved) in [
        (
            ids::CONNECTED_APPS_REQUEST_DECLINE,
            card_t::CONSENT_DENY_BUTTON,
            false,
        ),
        (
            ids::CONNECTED_APPS_REQUEST_APPROVE,
            card_t::CONSENT_APPROVE_BUTTON,
            true,
        ),
    ] {
        let btn = gtk::Button::builder().label(label).build();
        if approved {
            btn.add_css_class("suggested-action");
        }
        set_test_id(&btn, id);
        // A decline is a nest call exactly like an approval (the waiting
        // browser is owed a clean refusal), so both gate together.
        crate::offline_gate::declare_wire_kind(&btn, "fauna.bridges.atproto.resolve_consent");
        let ctx = Rc::clone(ctx);
        let consent_id_hex = consent.consent_id_hex.clone();
        btn.connect_clicked(move |_| {
            run(
                &ctx,
                Gesture::Resolve {
                    consent_id_hex: consent_id_hex.clone(),
                    approved,
                },
            )
        });
        actions.append(&btn);
    }
    card.append(&actions);

    let block = flat_button(t::BLOCK, ids::CONNECTED_APPS_REQUEST_BLOCK);
    block.set_halign(gtk::Align::End);
    crate::offline_gate::declare_wire_kind(&block, "fauna.oauth.consent.block_client");
    {
        let ctx = Rc::clone(ctx);
        let consent_id_hex = consent.consent_id_hex.clone();
        block.connect_clicked(move |_| run(&ctx, Gesture::Block(consent_id_hex.clone())));
    }
    card.append(&block);
    card
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::testid::widget_names;

    /// The page exposes every static (page-level) ui.yaml ID with no registered
    /// client. The per-row IDs (`connected-apps-item` + children, the request
    /// cards, the blocked rows) render only after an async read, so they are
    /// covered by the cross-app e2e (`test_connected_apps.py`), not here.
    #[test]
    fn connected_apps_page_exposes_static_ui_yaml_ids() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let (page, _refresh) = build_connected_apps_page();
            let names = widget_names(&page);
            for id in [
                "page-heading",
                "error-message",
                "connected-apps-connect-code",
                "connected-apps-connect-submit",
            ] {
                assert!(
                    names.iter().any(|n| n == id),
                    "missing widget id {id:?}; have {names:?}"
                );
            }
        });
    }
}
