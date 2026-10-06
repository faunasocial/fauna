//! The user-facing "Nests" page — per-user nest pairing (multi-homing) + the
//! v1 **nest-trust** facet.
//!
//! Where a user links one of their *own* nests to sync their account's content,
//! lists the nests their content lives on (their home nest + any linked nests),
//! unlinks them, and — on each nest row — sees a **trust facet**: what
//! content-processing that nest has been *trusted to read* (the grants the
//! client minted to it), with a per-row **Now / History** lens over the
//! client-authoritative signed grant-event log, per-grant renew/revoke, and the
//! honest bound of revocation. This is a **user** surface, not the admin shell;
//! the admin's only pairing control is the admin `admin-service-pairing-toggle`
//! (views/admin.rs § Services). Target state: `docs/goal/ui/nests.md` (page UX +
//! trust facet) + `docs/goal/behavior/linked-nests.md` (the linking half); wire
//! shape + capabilities: `docs/goal/architecture/nest/private-mode.md` § Pairing.
//!
//! Per priority #1/#2 this layer holds **no** pairing or trust logic — it is
//! dumb rendering of `LinkedNestsSnapshot` (home row + pairings, each with its
//! trust facet) + dispatch of `LinkedNestsAction` (Link / Unlink / Mint / Renew
//! / Revoke / SetLens / RevokeBackupSeal / RevokeBackupWriter). All sequencing —
//! parse the entered identity, holder discovery, the grant-log folds, the
//! Now/History projection, and **which nest each backup revoke is spoken to** —
//! lives in the shared `fauna_client_pair::LinkedNestsMachine`
//! (`libs/fauna-client-pair`), exposed over UniFFI/wasm so the other six apps
//! render the identical snapshot. The scope-first **mint** flow landed
//! 2026-07-13 (`nests.md` § Mint), the **backup trust rows** 2026-07-24
//! (`nests.md` § Trust facet — backup rows), and **retained-generation rows**
//! 2026-07-29 (`nests.md` § Trust facet — generation recovery, lifted from tui's
//! landed shape) — what the owner can roll back to at each destination once a
//! revoke has frozen a rogue source's writer grant but not undone the damage.
//! "capability"/"grant" stay internal — the UI says "trust to read …" (`nests.md`
//! § Naming).
//!
//! # Embedding + AT-SPI discoverability
//!
//! Like `settings/mail.rs`, the page is embedded in the status view
//! (`views/status.rs`): `navigate_to("settings")` reaches the status stack page,
//! NOT the separate `adw::PreferencesWindow` modal (which the state protocol
//! can't open). The trust facet uses plain `gtk::Label`s / `gtk::Button`s (their
//! `set_widget_name` IS AT-SPI-discoverable and carries their text), so the e2e
//! resolves `nest-trust-*` ids directly — no 1px marker labels needed (those are
//! only for adw row title/subtitle, which isn't discoverable). The add-a-nest
//! surface is an **inline reveal** (not a modal) so it stays in the reachable
//! tree. The nav sub-page slug is `nests` (the one id the shared e2e action
//! sends every app), matching the testids, i18n and page label.

use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;

use crate::async_helper::spawn_with_snapshot;

use fauna_client_pair::{
    ForwardQueueStatus, LinkedNestRow, LinkedNestStatus, LinkedNestsAction, LinkedNestsSnapshot,
    TrustBackupKind, TrustBackupRow, TrustBackupStatus, TrustFolder, TrustGenerationRow,
    TrustGenerationStatus, TrustGrantRow, TrustHistoryRow, TrustLens, TrustLiveness,
    TrustMintOption, TrustRestoreOutcome, TrustScope,
};

use crate::client::FaunaClient;
use crate::i18n::strings::nests as S;
use crate::testid::set_test_id;
use fauna_client_capabilities::view_model::CustodyRowView;

/// Widget handles the render + event closures need.
struct Widgets {
    add_button: gtk::Button,
    form_group: adw::PreferencesGroup,
    add_input: gtk::Entry,
    submit_button: gtk::Button,
    cancel_button: gtk::Button,
    error_label: gtk::Label,
    /// The page-level forward-queue group (`nests-forward-*`, `nests.md`
    /// § Forward queue) — visible only while the snapshot's queue is non-empty.
    forward_group: adw::PreferencesGroup,
    /// Its content, rebuilt wholesale on every snapshot.
    forward_container: gtk::Box,
    forward_block: RefCell<Option<gtk::Box>>,
    /// The vertical box that holds the per-nest `nests-item` rows (home first,
    /// then pairings). Rebuilt wholesale on every snapshot.
    list_container: gtk::Box,
    placeholder_row: adw::ActionRow,
    rows: RefCell<Vec<gtk::Box>>,
}

/// Everything the page's handlers + render need: the shared machine, the tokio
/// handle for async dispatch, and the widget handles. `Rc`-shared into closures.
struct Ctx {
    machine: Arc<fauna_client_pair::LinkedNestsMachine>,
    client: Rc<FaunaClient>,
    rt: tokio::runtime::Handle,
    w: Widgets,
    /// The custody fold's rows (`nests.md` § Trust facet — custody rows) — the
    /// NEST-anchored ones render here as their own `nests-item`s. Kept across
    /// a pass whose config read failed, so a transient never blanks live rows.
    custody_rows: RefCell<Vec<CustodyRowView>>,
    /// Who holds this account's generation-key escrow — the escrow-holder
    /// badge's source (`AccountStoreHandle::escrow_holders`). Kept across an
    /// unreadable pass for the same reason.
    escrow_holders: RefCell<Vec<[u8; 32]>>,
}

/// One Nests hydrate's results: the machine snapshot, plus the custody fold
/// and the escrow holders (`None` = unreadable this pass — keep the previous).
type Hydrated = (
    LinkedNestsSnapshot,
    Option<Vec<CustodyRowView>>,
    Option<Vec<[u8; 32]>>,
);

/// A left-aligned, ellipsized `gtk::Label` carrying `text` and a test `id`. The
/// trust facet's read-only fields are plain labels (discoverable + text-bearing,
/// unlike adw row title/subtitle). Ellipsized + width-capped so a long value
/// can't force the embedded page wide (see settings/mail.rs::value_marker).
fn field_label(id: &str, text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_use_markup(false);
    label.set_halign(gtk::Align::Start);
    label.set_xalign(0.0);
    label.set_wrap(false);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_max_width_chars(48);
    set_test_id(&label, id);
    label
}

/// Build the "Nests" preferences page. The returned closure re-hydrates the
/// machine (pairings + the home row's trust facet + mint-option catalog) — the
/// settings shell calls it whenever this becomes the visible sub-page, so the
/// page reflects state changed since login (a tier created in the Tiers tab, a
/// grant minted from another device) rather than the build-time snapshot; the
/// `subscription-settings`/`general` on-visible refresh idiom.
pub fn build_linked_nests_page() -> (adw::PreferencesPage, impl Fn() + 'static) {
    let page = adw::PreferencesPage::builder()
        .title(S::TITLE)
        .icon_name("network-server-symbolic")
        .build();

    // --- Top group: heading + add button + page-level error ---
    let top_group = adw::PreferencesGroup::builder()
        .title(S::TITLE)
        .description(S::DESCRIPTION)
        .build();
    // page-heading — a marker so AT-SPI resolves the global heading element.
    top_group.set_header_suffix(Some(&super::marker("page-heading")));

    let add_button = gtk::Button::builder()
        .label(S::ADD_BUTTON)
        .css_classes(["suggested-action"])
        .valign(gtk::Align::Center)
        .build();
    set_test_id(&add_button, ids::NESTS_ADD_BUTTON);
    let add_row = adw::ActionRow::builder()
        .title(S::ADD_BUTTON)
        .subtitle(S::AUTHORIZE_SUBTITLE)
        .activatable(false)
        .build();
    add_row.add_suffix(&add_button);
    top_group.add(&add_row);

    // error-message — page-level error label (Rule 2), hidden until set.
    let error_label = gtk::Label::builder().visible(false).build();
    error_label.add_css_class("error");
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    top_group.add(&error_label);
    page.add(&top_group);

    // --- Add form (inline reveal; hidden until the add button is clicked) ---
    let form_group = adw::PreferencesGroup::builder()
        .title(S::ADD_BUTTON)
        .visible(false)
        .build();

    let add_input = gtk::Entry::builder()
        .placeholder_text(S::ADD_INPUT_PLACEHOLDER)
        .hexpand(true)
        .build();
    set_test_id(&add_input, ids::NESTS_ADD_INPUT);
    // Enter in the input activates `submit_link` exactly like the submit
    // button below (both resolve to `LinkedNestsAction::Link`/`LinkBoth`,
    // which the shared machine always issues as `fauna.pair.add`).
    crate::offline_gate::declare_wire_kind(&add_input, "fauna.pair.add");
    let input_row = adw::ActionRow::builder()
        .title(S::NEST_TO_LINK)
        .activatable(false)
        .build();
    input_row.add_suffix(&add_input);
    form_group.add(&input_row);

    let submit_button = gtk::Button::builder()
        .label(S::ADD_SUBMIT)
        .css_classes(["suggested-action"])
        .valign(gtk::Align::Center)
        .build();
    set_test_id(&submit_button, ids::NESTS_ADD_SUBMIT_BUTTON);
    crate::offline_gate::declare_wire_kind(&submit_button, "fauna.pair.add");
    let cancel_button = gtk::Button::builder()
        .label(S::ADD_CANCEL)
        .valign(gtk::Align::Center)
        .build();
    set_test_id(&cancel_button, ids::NESTS_ADD_CANCEL_BUTTON);
    let buttons_row = adw::ActionRow::builder().activatable(false).build();
    buttons_row.add_suffix(&cancel_button);
    buttons_row.add_suffix(&submit_button);
    form_group.add(&buttons_row);
    page.add(&form_group);

    // --- Forward queue (page-level, conditional): after the add form, before
    // the nest rows (`nests.md` § Forward queue). Hidden until a snapshot
    // reports a non-empty queue.
    let forward_group = adw::PreferencesGroup::builder().visible(false).build();
    let forward_container = gtk::Box::new(gtk::Orientation::Vertical, 6);
    forward_group.add(&forward_container);
    page.add(&forward_group);

    // --- List group: placeholder + a container of indexed nests-item rows ---
    let list_group = adw::PreferencesGroup::builder()
        .title(S::LIST_TITLE)
        .build();
    let list_container = gtk::Box::new(gtk::Orientation::Vertical, 12);
    list_group.add(&list_container);
    let placeholder_row = adw::ActionRow::builder().title(S::EMPTY).build();
    list_group.add(&placeholder_row);
    page.add(&list_group);

    let widgets = Widgets {
        add_button,
        form_group,
        add_input,
        submit_button,
        cancel_button,
        error_label,
        forward_group,
        forward_container,
        forward_block: RefCell::new(None),
        list_container,
        placeholder_row,
        rows: RefCell::new(Vec::new()),
    };
    let refresh = wire_machine(widgets);
    (page, refresh)
}

/// Connect the page to the shared `LinkedNestsMachine`, hydrate on mount, and
/// wire every interaction (add button, form submit/cancel). Returns the
/// on-visible re-hydrate closure (a no-op — and the page stays at static
/// placeholders — when no client is registered, e.g. the unit test).
fn wire_machine(widgets: Widgets) -> impl Fn() + 'static {
    let client = match crate::settings::get_client() {
        Some(c) => c,
        None => return Box::new(|| {}) as Box<dyn Fn()>,
    };
    // The `fauna.pair.*` kinds are bearer, owner-scoped — the WS connection actor
    // scopes every call, so pairing itself needs no keypair. We build the machine
    // *with the mail relay-provisioning post-link hook AND the trust facet* (both
    // need the keypair — the hook to provision the relayed mailbox reusing the
    // fleet MSEK, the trust seams to read/sign the grant-event log), falling back
    // to the plain machine if the identity can't be derived — the page still
    // lists / links / unlinks, only the mailbox auto-provision + trust facet are
    // skipped.
    let machine = Arc::new(
        match crate::mail_glue::build_linked_nests_machine_with_mail_relay_and_trust(&client) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(
                    target: "fauna_linux",
                    "nests: trust/mail-relay seams unavailable ({e}); building plain machine"
                );
                fauna_client_pair::build_linked_nests_machine(client.nest_rpc().clone())
            }
        },
    );

    let ctx = Rc::new(Ctx {
        machine,
        rt: client.runtime_handle(),
        client,
        w: widgets,
        custody_rows: RefCell::new(Vec::new()),
        escrow_holders: RefCell::new(Vec::new()),
    });

    // Hydrate on mount: list the owner's pairings + the home nest's trust facet.
    // The hydrate's refresh also runs the auto-renew sweep, so a blessed nest's
    // due grants renew as the app comes up (`nests.md` § Expiry / renewal →
    // *Duration and blessing*: at app foreground).
    hydrate_and_render(&ctx);

    // …and then every `AUTO_RENEW_CHECK_SECS`, whichever page the user sits
    // on: this page is built once with the settings shell, so its timer is
    // app-wide (tui's `nests_renew_tick`).
    {
        let ctx = Rc::clone(&ctx);
        gtk::glib::timeout_add_seconds_local(
            fauna_client_capabilities::view_model::AUTO_RENEW_CHECK_SECS as u32,
            move || {
                dispatch_and_render(&ctx, LinkedNestsAction::AutoRenew, false);
                gtk::glib::ControlFlow::Continue
            },
        );
    }

    // Add button → reveal the form, cleared.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.add_button.clone().connect_clicked(move |_| {
            ctx.w.add_input.set_text("");
            ctx.w.form_group.set_visible(true);
        });
    }

    // Cancel → hide the form.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.cancel_button.clone().connect_clicked(move |_| {
            ctx.w.form_group.set_visible(false);
        });
    }

    // Submit (button or Enter in the entry) → Link.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .submit_button
            .clone()
            .connect_clicked(move |_| submit_link(&ctx));
    }
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .add_input
            .clone()
            .connect_activate(move |_| submit_link(&ctx));
    }

    // The on-visible re-hydrate: same full refresh as mount.
    Box::new(move || hydrate_and_render(&ctx)) as Box<dyn Fn()>
}

/// Read the entered value and dispatch the link action it resolves to, with the
/// default full self-sync capability set (empty `capabilities` → the machine
/// fills it). A nest **address** seeds both ends in one action (`LinkBoth`); a
/// bare 64-hex **identity** authorizes that one nest (`Link`, the out-of-band
/// path). Routing lives in shared Rust (`classify_link_input`) so every app
/// resolves the same input identically (priority #2). The add re-lists on
/// success; the form closes only when no error came back.
fn submit_link(ctx: &Rc<Ctx>) {
    let raw = ctx.w.add_input.text().trim().to_string();
    let action = match fauna_client_pair::classify_link_input(&raw) {
        fauna_client_pair::LinkInput::NestUrl { nest_url } => LinkedNestsAction::LinkBoth {
            other_nest_url: nest_url,
            capabilities: Vec::new(),
            expires_at: None,
            label: None,
        },
        fauna_client_pair::LinkInput::NestId { nest_id } => LinkedNestsAction::Link {
            nest_id,
            capabilities: Vec::new(),
            expires_at: None,
            label: None,
            nest_url: None,
        },
    };
    let close_form_on_success = true;
    dispatch_and_render(ctx, action, close_form_on_success);
}

/// Run `machine.hydrate()` on the tokio runtime (retrying while the WS socket
/// comes up after login), then render the snapshot on the GTK main thread.
fn hydrate_and_render(ctx: &Rc<Ctx>) {
    let machine = Arc::clone(&ctx.machine);
    let nest = Arc::clone(ctx.client.nest_rpc());
    let store = crate::account_runtime::handle();
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            // The WS-RPC connection comes up shortly after login, but the embedded
            // settings page can mount first — the first `fauna.pair.list` then fails
            // with RpcDisconnected. Retry a few times so the page hydrates as soon
            // as the socket is ready (mirrors settings/mail.rs).
            let _ = machine.hydrate().await;
            // The custodian-NEST rows render from the same custody fold the
            // Devices page paints (its complement), and the escrow-holder
            // badge from the shared `escrow_holders` store read — both on this
            // page's own nav edge, like tui's Nests hydrate.
            let custody = fauna_client_custody::load_custody_facet(nest, store.clone())
                .await
                .map(|f| f.rows);
            let escrow = match store {
                Some(store) => store.escrow_holders().await.ok(),
                None => None,
            };
            (machine.snapshot(), custody, escrow) as Hydrated
        },
        move |(snap, custody, escrow)| {
            if let Some(rows) = custody {
                *ctx_render.custody_rows.borrow_mut() = rows;
            }
            if let Some(holders) = escrow {
                *ctx_render.escrow_holders.borrow_mut() = holders;
            }
            render(&ctx_render, &snap);
        },
    );
}

/// Revoke a custodian nest's custody (`nest-trust-custody-revoke-button`) —
/// the same shared act the Devices page's `custody-holder-revoke-button` runs
/// (the nest's revoke BEFORE the signed record lives in
/// `fauna_client_custody::run_custody_act`), then re-hydrate so the row drops.
/// An error lands on the page's `error-message`, never a silent drop.
fn revoke_custody_nest(ctx: &Rc<Ctx>, grant_id: Vec<u8>, holder: Option<[u8; 32]>) {
    let nest = Arc::clone(ctx.client.nest_rpc());
    let secret = ctx.client.secret_bytes();
    let store = crate::account_runtime::handle();
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            let (_facet, error) = fauna_client_custody::run_custody_act(
                fauna_client_custody::CustodyCtx {
                    nest,
                    secret,
                    store,
                    session: None,
                },
                fauna_client_custody::CustodyAct::Revoke { grant_id, holder },
            )
            .await;
            error
        },
        move |error| {
            if let Some(e) = error.as_deref() {
                super::render_error_label(&ctx_render.w.error_label, Some(e));
            }
            hydrate_and_render(&ctx_render);
        },
    );
}

/// Dispatch an action on the tokio runtime, then render the resulting snapshot.
/// When `close_form_on_success` is set, an error-free result hides the add form.
fn dispatch_and_render(ctx: &Rc<Ctx>, action: LinkedNestsAction, close_form_on_success: bool) {
    let machine = Arc::clone(&ctx.machine);
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            let _ = machine.dispatch(action).await;
            machine.snapshot()
        },
        move |snap| {
            render(&ctx_render, &snap);
            if close_form_on_success && snap.error.is_none() {
                ctx_render.w.form_group.set_visible(false);
            }
        },
    );
}

/// Render a `LinkedNestsSnapshot` into the page widgets (GTK main thread). The
/// home nest row (`nests.md` § Layout — the connected nest) renders first, then
/// the pairings; each carries its trust facet.
fn render(ctx: &Rc<Ctx>, snap: &LinkedNestsSnapshot) {
    let w = &ctx.w;

    // A mutation in flight disables the add button so the user can't double-fire.
    w.add_button
        .set_sensitive(!matches!(snap.status, LinkedNestStatus::Working));

    // The page's only message channel — a restore's outcome (Restored /
    // PastRecoveryWindow) is NOT an error and rides its own dedicated
    // `nest-trust-generation-notice` element instead (`build_trust_facet`
    // below, ratified 2026-07-29). Only a genuinely failed call reaches this.
    super::render_error_label(&w.error_label, snap.error.as_deref());

    // Forward queue: rebuilt per snapshot, painted only while non-empty — so an
    // ordinary Nests page carries none of the `nests-forward-*` ids.
    {
        let mut block = w.forward_block.borrow_mut();
        if let Some(old) = block.take() {
            w.forward_container.remove(&old);
        }
        let queue = snap.forward_queue.as_ref().filter(|q| q.queued > 0);
        if let Some(queue) = queue {
            let (b, retry, discard) = build_forward_queue(queue);
            {
                let ctx = Rc::clone(ctx);
                retry.connect_clicked(move |_| {
                    dispatch_and_render(&ctx, LinkedNestsAction::RetryForwards, false);
                });
            }
            {
                let ctx = Rc::clone(ctx);
                discard.connect_clicked(move |_| {
                    dispatch_and_render(&ctx, LinkedNestsAction::DiscardForwards, false);
                });
            }
            w.forward_container.append(&b);
            *block = Some(b);
        }
        w.forward_group.set_visible(queue.is_some());
    }

    // Nest list: tear down the previous rows, rebuild home-first then pairings.
    {
        let mut rows = w.rows.borrow_mut();
        for row in rows.drain(..) {
            w.list_container.remove(&row);
        }
        let escrow_holders = ctx.escrow_holders.borrow();
        for nest in snap.home.iter().chain(snap.pairings.iter()) {
            let item = build_nest_item(ctx, nest, snap.restore_outcome, &escrow_holders);
            w.list_container.append(&item);
            rows.push(item);
        }
        // Custodian nests (`nests.md` § Trust facet — custody rows): one
        // `nests-item` per NEST-anchored custody, after the linked rows (the
        // tui order). Device-anchored custody stays on the Devices page.
        for custody in ctx
            .custody_rows
            .borrow()
            .iter()
            .filter(|c| c.custodian_nest_url.is_some())
        {
            let ctx_revoke = Rc::clone(ctx);
            let item = build_custody_nest_item(custody, move |grant_id, holder| {
                revoke_custody_nest(&ctx_revoke, grant_id, holder)
            });
            w.list_container.append(&item);
            rows.push(item);
        }
    }
    let no_custody_nests = !ctx
        .custody_rows
        .borrow()
        .iter()
        .any(|c| c.custodian_nest_url.is_some());
    w.placeholder_row
        .set_visible(snap.home.is_none() && snap.pairings.is_empty() && no_custody_nests);
}

/// One custodian-NEST `nests-item` row + its `nest-trust-custody-*` family —
/// tui's `push_custody_nest_item`, field for field. The copy is the shared
/// `devices.custody_*` strings the Devices `custody-holder-card` paints
/// (receipt three-state honesty, held bytes, revoke beside its REQUIRED
/// honest-bound note); the scope line is trust vocabulary only. `on_revoke`
/// receives the grant id + accept-bound custodian key, never an index.
fn build_custody_nest_item(
    row: &CustodyRowView,
    on_revoke: impl Fn(Vec<u8>, Option<[u8; 32]>) + 'static,
) -> gtk::Box {
    use crate::i18n::strings::devices as D;
    use fauna_client_capabilities::view_model::ReceiptState;

    let item = gtk::Box::new(gtk::Orientation::Vertical, 4);
    set_test_id(&item, ids::NESTS_ITEM);
    item.set_accessible_role(gtk::AccessibleRole::Group);
    item.add_css_class("card");
    item.set_margin_top(4);
    item.set_margin_bottom(4);

    let host = fauna_core::format::short_id(&hex::encode(row.host));
    let label_lbl = field_label(ids::NESTS_ITEM_LABEL, &S::custody_nest_label(&host));
    label_lbl.add_css_class("heading");
    item.append(&label_lbl);

    // The item anchor: a Group container carrying the family, its own line the
    // honest-bound revoke copy (the bound stated beside the control).
    let custody = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&custody, ids::NEST_TRUST_CUSTODY_ITEM);

    custody.append(&field_label(
        ids::NEST_TRUST_CUSTODY_SCOPE,
        D::CUSTODY_HOLDER_SCOPE,
    ));

    // Three states, three strings — never collapsed, never empty.
    let status = field_label(
        ids::NEST_TRUST_CUSTODY_RECEIPT_STATUS,
        &crate::i18n::custody_receipt_status(
            row.receipt_state,
            row.receipt.as_ref().map(|r| r.attested_at_micros),
        ),
    );
    status.add_css_class(match row.receipt_state {
        ReceiptState::Fresh => "success",
        ReceiptState::Stale => "warning",
        ReceiptState::NoReceiptYet => "dim-label",
    });
    custody.append(&status);

    let bytes = field_label(
        ids::NEST_TRUST_CUSTODY_HELD_BYTES,
        &crate::i18n::custody_held_bytes(row.receipt.as_ref()),
    );
    bytes.add_css_class("caption");
    custody.append(&bytes);

    let bound = gtk::Label::builder()
        .label(D::CUSTODY_REVOKE_BOUND_NOTE)
        .halign(gtk::Align::Start)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["caption", "dim-label"])
        .build();
    custody.append(&bound);

    let revoke = gtk::Button::builder()
        .label(D::CUSTODY_REVOKE)
        .halign(gtk::Align::Start)
        .css_classes(["destructive-action"])
        .build();
    // A pending ceremony has minted nothing to revoke yet.
    revoke.set_sensitive(!row.pending);
    set_test_id(&revoke, ids::NEST_TRUST_CUSTODY_REVOKE_BUTTON);
    crate::offline_gate::declare_wire_kind(&revoke, "fauna.capabilities.revoke");
    {
        let grant_id = row.grant_id.clone();
        let holder = row.custodian_key;
        revoke.connect_clicked(move |_| on_revoke(grant_id.clone(), holder));
    }
    custody.append(&revoke);

    item.append(&custody);
    item
}

/// The page-level forward-queue block (`nests-forward-*`): the count, the stuck
/// half's what-to-check when any entry is past the retry ceiling, the nest's
/// own latest failure when one was recorded, and the two actions — mirroring
/// tui's `push_forward_queue` field for field. Returns the block plus the
/// Retry / Stop-forwarding buttons for the caller to wire.
///
/// The reason is partly relay-chosen text (`private-mode.md` § Post
/// Forwarding): the shared projection already control-stripped it, and it is
/// painted here through a plain, markup-off label — `<b>` stays literal.
fn build_forward_queue(queue: &ForwardQueueStatus) -> (gtk::Box, gtk::Button, gtk::Button) {
    let block = gtk::Box::new(gtk::Orientation::Vertical, 6);

    let mut summary = S::forward_queue_summary(&queue.queued.to_string());
    if queue.stuck > 0 {
        summary.push(' ');
        summary.push_str(&S::forward_queue_stuck(&queue.stuck.to_string()));
    }
    let summary_lbl = gtk::Label::new(Some(&summary));
    summary_lbl.set_use_markup(false);
    summary_lbl.set_halign(gtk::Align::Start);
    summary_lbl.set_xalign(0.0);
    summary_lbl.set_wrap(true);
    set_test_id(&summary_lbl, ids::NESTS_FORWARD_QUEUE);
    block.append(&summary_lbl);

    if let Some(error) = queue.last_error.as_deref().filter(|e| !e.is_empty()) {
        let reason = field_label(
            ids::NESTS_FORWARD_QUEUE_REASON,
            &S::forward_queue_last_error(error),
        );
        reason.add_css_class("dim-label");
        block.append(&reason);
    }

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let retry = gtk::Button::builder()
        .label(S::FORWARD_RETRY)
        .halign(gtk::Align::Start)
        .build();
    set_test_id(&retry, ids::NESTS_FORWARD_RETRY_BUTTON);
    crate::offline_gate::declare_wire_kind(&retry, "fauna.pair.forward_retry");
    let discard = gtk::Button::builder()
        .label(S::FORWARD_DISCARD)
        .halign(gtk::Align::Start)
        .css_classes(["destructive-action"])
        .build();
    set_test_id(&discard, ids::NESTS_FORWARD_DISCARD_BUTTON);
    crate::offline_gate::declare_wire_kind(&discard, "fauna.pair.forward_discard");
    actions.append(&retry);
    actions.append(&discard);
    block.append(&actions);

    (block, retry, discard)
}

/// Build one `nests-item` row from a `LinkedNestRow`: the identity line, then the
/// trust facet (per-row Now/History lens + grants / history / empty state). The
/// home row (`is_home`) carries no Unlink and no sync-caps/expiry lines — it is
/// the user's own connected nest, not a pairing (`nests.md` § Layout).
fn build_nest_item(
    ctx: &Rc<Ctx>,
    row: &LinkedNestRow,
    restore_outcome: Option<TrustRestoreOutcome>,
    escrow_holders: &[[u8; 32]],
) -> gtk::Box {
    let item = gtk::Box::new(gtk::Orientation::Vertical, 4);
    // nests-item — the indexed row container. A named container Box carries an
    // explicit Group role so it stays discoverable by id (plain Boxes default to
    // Generic; ui-actual-linux § test_api — same idiom as folders.rs rows).
    set_test_id(&item, ids::NESTS_ITEM);
    item.set_accessible_role(gtk::AccessibleRole::Group);
    item.add_css_class("card");
    item.set_margin_top(4);
    item.set_margin_bottom(4);

    // --- identity line ---
    let abbreviated = fauna_core::format::short_id(&row.nest_id);
    let label_text = row
        .label
        .clone()
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| abbreviated.clone());

    let label_lbl = field_label("nests-item-label", &label_text);
    label_lbl.add_css_class("heading");
    item.append(&label_lbl);

    let id_lbl = field_label("nests-item-nest-id", &abbreviated);
    id_lbl.add_css_class("dim-label");
    id_lbl.add_css_class("caption");
    item.append(&id_lbl);

    // The escrow-holder role badge (participants.md § The participant model →
    // Roles): this nest holds the account's generation-key escrow. Derived
    // from recorded escrow receipts, never asserted by the nest itself.
    if holds_escrow(&row.nest_id, escrow_holders) {
        let badge = field_label(ids::PARTICIPANT_ESCROW_HOLDER_BADGE, S::ESCROW_HOLDER_BADGE);
        badge.add_css_class("caption");
        badge.add_css_class("accent");
        item.append(&badge);
    }

    if !row.is_home {
        let caps = row
            .capability_labels
            .iter()
            .map(|l| l.clone().resolve(crate::i18n::strings::lookup))
            .collect::<Vec<_>>()
            .join(", ");
        let caps_lbl = field_label("nests-item-capabilities", &caps);
        caps_lbl.add_css_class("caption");
        item.append(&caps_lbl);

        let expiry = match row.expires_at {
            Some(_) => S::EXPIRY_LABEL,
            None => S::EXPIRY_NEVER,
        };
        let expiry_lbl = field_label("nests-item-expiry", expiry);
        expiry_lbl.add_css_class("caption");
        item.append(&expiry_lbl);

        let unlink_btn = gtk::Button::builder()
            .label(S::UNLINK)
            .halign(gtk::Align::Start)
            .css_classes(["destructive-action"])
            .build();
        set_test_id(&unlink_btn, ids::NESTS_ITEM_UNLINK_BUTTON);
        crate::offline_gate::declare_wire_kind(&unlink_btn, "fauna.pair.revoke");
        {
            let ctx = Rc::clone(ctx);
            let nest_id = row.nest_id.clone();
            unlink_btn.connect_clicked(move |_| {
                dispatch_and_render(
                    &ctx,
                    LinkedNestsAction::Unlink {
                        nest_id: nest_id.clone(),
                    },
                    false,
                );
            });
        }
        item.append(&unlink_btn);
    }

    // --- trust facet: per-row Now/History lens (nests.md § Trust facet) ---
    item.append(&build_trust_facet(ctx, row, restore_outcome));
    item
}

/// Whether the nest row `nest_id` (hex) is among the recorded escrow holders.
fn holds_escrow(nest_id: &str, escrow_holders: &[[u8; 32]]) -> bool {
    fauna_core::hex32::decode(nest_id).is_ok_and(|id| escrow_holders.contains(&id))
}

/// Build the trust facet for one nest row: the always-present Now/History lens
/// toggle, then the active lens's content — the grant list (Now, with a
/// `nest-trust-empty` state when the nest holds none) or the grant-event
/// timeline (History). Rebuilt per snapshot; `SetLens` flips `row.lens` locally
/// (no nest round-trip) and re-renders. `restore_outcome` is the SNAPSHOT's
/// last-action state (`nests.md` § Trust facet — generation recovery), not
/// per-row — only the home row renders it, since generation rows only exist
/// there.
fn build_trust_facet(
    ctx: &Rc<Ctx>,
    row: &LinkedNestRow,
    restore_outcome: Option<TrustRestoreOutcome>,
) -> gtk::Box {
    let facet = gtk::Box::new(gtk::Orientation::Vertical, 6);
    facet.set_margin_top(4);

    // Lens toggle — always present (both lenses reachable, `nests.md:29`).
    let toggle = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let now_btn = gtk::Button::with_label(S::VIEW_NOW);
    set_test_id(&now_btn, ids::NEST_TRUST_VIEW_NOW);
    let hist_btn = gtk::Button::with_label(S::VIEW_HISTORY);
    set_test_id(&hist_btn, ids::NEST_TRUST_VIEW_HISTORY);
    // The active lens's button reads as selected.
    match row.lens {
        TrustLens::Now => now_btn.add_css_class("suggested-action"),
        TrustLens::History => hist_btn.add_css_class("suggested-action"),
    }
    {
        let ctx = Rc::clone(ctx);
        let nest_id = row.nest_id.clone();
        now_btn.connect_clicked(move |_| {
            dispatch_and_render(
                &ctx,
                LinkedNestsAction::SetLens {
                    nest_id: nest_id.clone(),
                    lens: TrustLens::Now,
                },
                false,
            );
        });
    }
    {
        let ctx = Rc::clone(ctx);
        let nest_id = row.nest_id.clone();
        hist_btn.connect_clicked(move |_| {
            dispatch_and_render(
                &ctx,
                LinkedNestsAction::SetLens {
                    nest_id: nest_id.clone(),
                    lens: TrustLens::History,
                },
                false,
            );
        });
    }
    toggle.append(&now_btn);
    toggle.append(&hist_btn);
    facet.append(&toggle);

    match row.lens {
        TrustLens::Now => {
            // `nest-trust-empty` means "this nest is trusted with nothing", so a
            // backup row suppresses it even with zero content grants
            // (`nests.md:99`, ratified + user-approved) — a nest
            // that seals and uploads your messages is plainly trusted, and
            // rendering "not trusted to read anything" directly above "Backs up
            // your messages for you" states the opposite of the row beneath it.
            if row.trust_grants.is_empty() && row.trust_backups.is_empty() {
                // The explicit "not trusted to read anything" empty state — a
                // Now-lens affordance (honest, not a broken/blank facet).
                let empty = field_label("nest-trust-empty", S::NOT_TRUSTED);
                empty.set_wrap(true);
                empty.set_ellipsize(gtk::pango::EllipsizeMode::None);
                empty.add_css_class("dim-label");
                facet.append(&empty);
            } else {
                let grant_list = gtk::Box::new(gtk::Orientation::Vertical, 6);
                set_test_id(&grant_list, ids::NEST_TRUST_GRANT_LIST);
                grant_list.set_accessible_role(gtk::AccessibleRole::Group);
                for g in &row.trust_grants {
                    let ctx = Rc::clone(ctx);
                    grant_list.append(&build_grant_item(g, move |action| {
                        dispatch_and_render(&ctx, action, false)
                    }));
                }
                facet.append(&grant_list);
            }
            // Backup trust rows (nests.md § Trust facet — backup rows, ratified
            // 2026-07-24) — AFTER the content-processing grant rows, home row
            // only (the shared machine populates them nowhere else, since both
            // grants empower the source nest). Outside the branch above because
            // they render alongside *either* arm: next to the grant list when
            // content grants exist, and on their own when none do (the empty
            // state is suppressed in that case — see the condition above).
            for b in &row.trust_backups {
                facet.append(&build_backup_item(ctx, b));
            }
            // Retained generations (nests.md § Trust facet — generation
            // recovery, ratified 2026-07-29) — AFTER the backup trust rows, on
            // their own home row: one surface, so "who may write here" and
            // "what can I roll back" read together (`nests.md:113`). Same
            // both-arms placement as the backup rows above.
            for g in &row.trust_generations {
                facet.append(&build_generation_item(ctx, g));
            }
            // The restore-outcome notice (`nest-trust-generation-notice`,
            // ratified 2026-07-29) — home-row-scoped, NOT per-row: a
            // restore's outcome describes the page's last action, not any
            // one generation row. Registered whenever the home row's Now
            // lens renders, EMPTY until a restore resolves — the leaf set
            // does not vary. Distinct from `error-message`: only a
            // genuinely failed call reaches that; `PastRecoveryWindow` is a
            // product state, never an error.
            if row.is_home {
                facet.append(&field_label(
                    "nest-trust-generation-notice",
                    generation_notice_text(restore_outcome),
                ));
            }
            // Mint flow (scope-first picker, nests.md § Mint, ratified
            // 2026-07-13) — rendered after the grant list / empty state, and
            // only when the shared option catalog is non-empty (an empty
            // catalog means nothing derivable or no discoverable holder —
            // never a picker that can only error).
            if row.is_home {
                facet.append(&build_blessed_toggle(ctx, row));
            }
            if !row.mint_options.is_empty() {
                facet.append(&build_mint_flow(ctx, row));
            }
        }
        TrustLens::History => {
            let history_list = gtk::Box::new(gtk::Orientation::Vertical, 4);
            set_test_id(&history_list, ids::NEST_TRUST_HISTORY_LIST);
            history_list.set_accessible_role(gtk::AccessibleRole::Group);
            for h in &row.trust_history {
                let line = history_line(h);
                let hlbl = field_label("nest-trust-history-item", &line);
                hlbl.set_wrap(true);
                hlbl.set_ellipsize(gtk::pango::EllipsizeMode::None);
                hlbl.add_css_class("caption");
                history_list.append(&hlbl);
            }
            facet.append(&history_list);
        }
    }
    facet
}

/// Build the mint flow for one nest row (`nest-trust-grant-mint-button` →
/// `nest-trust-mint-scope-select` [→ `nest-trust-mint-holder-select`] →
/// `nest-trust-mint-confirm-button`; nests.md § Mint, scope-first design
/// ratified 2026-07-13). The scope select's options are the shared
/// `LinkedNestRow.mint_options` catalog verbatim (one use-case option each,
/// labeled shell-side — priority #2); the holder is derived from the chosen
/// option, and the holder select renders only when an option lists more than
/// one candidate (the ambiguity case; every option derives exactly one today).
/// Confirm dispatches `Mint{nest_id, holder_bridge_id, scope}` and the
/// re-render collapses the form.
fn build_mint_flow(ctx: &Rc<Ctx>, row: &LinkedNestRow) -> gtk::Box {
    let mint_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    mint_box.set_margin_top(4);

    let mint_btn = gtk::Button::with_label(S::MINT_BUTTON);
    mint_btn.set_halign(gtk::Align::Start);
    set_test_id(&mint_btn, ids::NEST_TRUST_GRANT_MINT_BUTTON);
    mint_box.append(&mint_btn);

    // The form, revealed by the mint button.
    let form = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    form.set_visible(false);

    // Scope select: option 0 is the placeholder; option i+1 = mint_options[i].
    let mut labels: Vec<String> = vec![S::MINT_SCOPE_PLACEHOLDER.to_string()];
    labels.extend(row.mint_options.iter().map(mint_option_label));
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let scope_dd = gtk::DropDown::new(
        Some(gtk::StringList::new(&label_refs)),
        gtk::Expression::NONE,
    );
    set_test_id(&scope_dd, ids::NEST_TRUST_MINT_SCOPE_SELECT);
    form.append(&scope_dd);

    // Conditional holder select — populated + shown only when the chosen
    // option has >1 candidate. Candidates are the stable holder names
    // (`bridge_id`, e.g. "web-serve"); a localized holder-label catalog
    // arrives with the first deployment that actually hits this arm.
    let holder_dd = gtk::DropDown::new(
        Some(gtk::StringList::new(&[] as &[&str])),
        gtk::Expression::NONE,
    );
    holder_dd.set_visible(false);
    set_test_id(&holder_dd, ids::NEST_TRUST_MINT_HOLDER_SELECT);
    form.append(&holder_dd);

    let duration_dd = build_duration_select(row);
    form.append(&duration_dd);

    let confirm_btn = gtk::Button::with_label(S::MINT_CONFIRM);
    confirm_btn.add_css_class("suggested-action");
    confirm_btn.set_sensitive(false);
    set_test_id(&confirm_btn, ids::NEST_TRUST_MINT_CONFIRM_BUTTON);
    crate::offline_gate::declare_wire_kind(&confirm_btn, "fauna.capabilities.mint");
    form.append(&confirm_btn);
    mint_box.append(&form);

    {
        let form = form.clone();
        mint_btn.connect_clicked(move |_| {
            form.set_visible(true);
        });
    }
    {
        let options = row.mint_options.clone();
        let holder_dd = holder_dd.clone();
        let confirm_btn = confirm_btn.clone();
        scope_dd.connect_selected_notify(move |dd| {
            let idx = dd.selected();
            let picked = (idx != gtk::INVALID_LIST_POSITION && idx > 0)
                .then(|| &options[(idx - 1) as usize]);
            confirm_btn.set_sensitive(picked.is_some());
            match picked {
                Some(o) if o.holder_candidates.len() > 1 => {
                    let cands: Vec<&str> = o.holder_candidates.iter().map(String::as_str).collect();
                    holder_dd.set_model(Some(&gtk::StringList::new(&cands)));
                    holder_dd.set_selected(0);
                    holder_dd.set_visible(true);
                }
                _ => holder_dd.set_visible(false),
            }
        });
    }
    {
        let ctx = Rc::clone(ctx);
        let nest_id = row.nest_id.clone();
        let options = row.mint_options.clone();
        let scope_dd = scope_dd.clone();
        let default_duration = row.mint_default_duration;
        confirm_btn.connect_clicked(move |_| {
            let idx = scope_dd.selected();
            if idx == gtk::INVALID_LIST_POSITION || idx == 0 {
                return;
            }
            let option = &options[(idx - 1) as usize];
            // Derived holder: the single candidate; ambiguity → the holder
            // select's pick (visible iff >1 candidate).
            let holder_bridge_id = if option.holder_candidates.len() > 1 {
                let h = holder_dd.selected();
                if h == gtk::INVALID_LIST_POSITION {
                    return;
                }
                option.holder_candidates[h as usize].clone()
            } else {
                option.holder_candidates[0].clone()
            };
            dispatch_and_render(
                &ctx,
                LinkedNestsAction::Mint {
                    nest_id: nest_id.clone(),
                    holder_bridge_id,
                    scope: option.scope.clone(),
                    // The owner's pick — the row's default until they choose.
                    duration: Some(selected_duration(&duration_dd, default_duration)),
                },
                false,
            );
        });
    }
    mint_box
}

/// How long the new trust lasts (`nest-trust-mint-duration-select`, `nests.md`
/// § Expiry / renewal → *Duration and blessing*): the shared option list,
/// labeled through the shared `duration_label`, pre-selecting the row's
/// `mint_default_duration` (the standing window on a blessed nest, hours
/// otherwise) — tui's `push_mint_flow` select, field for field.
fn build_duration_select(row: &LinkedNestRow) -> gtk::DropDown {
    let options = fauna_client_pair::mint_duration_options();
    let labels: Vec<String> = options.iter().copied().map(duration_label).collect();
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let dd = gtk::DropDown::new(
        Some(gtk::StringList::new(&label_refs)),
        gtk::Expression::NONE,
    );
    let default = options
        .iter()
        .position(|d| *d == row.mint_default_duration)
        .unwrap_or(0);
    dd.set_selected(default as u32);
    set_test_id(&dd, ids::NEST_TRUST_MINT_DURATION_SELECT);
    dd
}

/// The duration the select shows, or `fallback` when nothing is selected.
fn selected_duration(
    dd: &gtk::DropDown,
    fallback: fauna_client_pair::TrustGrantDuration,
) -> fauna_client_pair::TrustGrantDuration {
    fauna_client_pair::mint_duration_options()
        .get(dd.selected() as usize)
        .copied()
        .unwrap_or(fallback)
}

/// A grant duration's label — the shared `duration_label` decision, resolved.
fn duration_label(d: fauna_client_pair::TrustGrantDuration) -> String {
    fauna_client_pair::duration_label(d).resolve(crate::i18n::strings::lookup)
}

/// The per-nest blessing (`nest-trust-blessed-toggle`, `nests.md` § Expiry /
/// renewal → *Duration and blessing*) — home row only, like the rest of the
/// facet. `state` mirrors the checkbox for a driver (the toggle convention);
/// toggling dispatches `SetBlessed`, a `fauna.state.blessed-nests` write (`fauna.account.state.put`,
/// offline-safe — so no offline gate).
fn build_blessed_toggle(ctx: &Rc<Ctx>, row: &LinkedNestRow) -> gtk::CheckButton {
    let toggle = gtk::CheckButton::with_label(S::BLESSED_TOGGLE);
    toggle.set_active(row.blessed);
    set_test_id(&toggle, ids::NEST_TRUST_BLESSED_TOGGLE);
    crate::testid::set_test_attr(&toggle, "state", if row.blessed { "on" } else { "off" });
    let ctx = Rc::clone(ctx);
    let nest_id = row.nest_id.clone();
    toggle.connect_toggled(move |t| {
        dispatch_and_render(
            &ctx,
            LinkedNestsAction::SetBlessed {
                nest_id: nest_id.clone(),
                blessed: t.is_active(),
            },
            false,
        );
    });
    toggle
}

/// `nests.mint_option_*`; the paywalled option carries its tier via the
/// `{tier}` named placeholder. Thin `resolve()` wrapper over the shared
/// `fauna_client_pair::mint_option_label` mapping (priority #1/#2).
fn mint_option_label(o: &TrustMintOption) -> String {
    fauna_client_pair::mint_option_label(o).resolve(crate::i18n::strings::lookup)
}

/// Build one backup trust row (`nest-trust-backup-item`) for the Now lens on the
/// home nest's row: the scope line, when the trust was given, the row state, the
/// REQUIRED honest-bound copy, and the freeze-the-backup affordance
/// (`nests.md` § Trust facet — backup rows).
///
/// Two row kinds share the component (`nests.md:63`): the seal grant, revoked at
/// the source nest, and one writer row per destination, revoked **at the
/// destination** — the shared machine routes each `nest-trust-backup-revoke`
/// press to the right nest, so this layer only names which row was pressed.
///
/// Deliberately no lasts-until / renew / History twin: both grants are standing
/// live nest reads, not folds of the signed grant-event log (`nests.md:99`).
fn build_backup_item(ctx: &Rc<Ctx>, backup: &TrustBackupRow) -> gtk::Box {
    let bi = gtk::Box::new(gtk::Orientation::Vertical, 2);
    set_test_id(&bi, ids::NEST_TRUST_BACKUP_ITEM);
    bi.set_accessible_role(gtk::AccessibleRole::Group);
    bi.add_css_class("card");
    bi.set_margin_top(2);
    bi.set_margin_bottom(2);

    let scope_text = match backup.kind {
        TrustBackupKind::Seal => S::BACKUP_SCOPE_SEAL.to_string(),
        TrustBackupKind::Writer => S::backup_scope_writer(&backup.destination_label),
    };
    let scope_lbl = field_label("nest-trust-backup-scope", &scope_text);
    scope_lbl.add_css_class("heading");
    bi.append(&scope_lbl);

    // `nest-trust-backup-since` renders EMPTY on the seal row — that grant
    // carries no timestamp on the wire (`nests.md:67`). The element is still
    // present so the row's leaf set doesn't vary by kind.
    let since_text = match backup.since {
        Some(at) => format!(
            "{} {}",
            S::BACKUP_SINCE,
            fauna_core::format::format_unix_local(at)
        ),
        None => String::new(),
    };
    let since_lbl = field_label("nest-trust-backup-since", &since_text);
    since_lbl.add_css_class("caption");
    bi.append(&since_lbl);

    let status_lbl = field_label(
        "nest-trust-backup-status",
        &backup_status_label(backup.status),
    );
    status_lbl.add_css_class("caption");
    bi.append(&status_lbl);

    // REQUIRED honest-bound copy — revoking freezes only NEW writes; custody
    // already held remains until the holder reclaims it. Never over-promise.
    let bound_note = match backup.kind {
        TrustBackupKind::Seal => S::BACKUP_BOUND_NOTE_SEAL,
        TrustBackupKind::Writer => S::BACKUP_BOUND_NOTE_WRITER,
    };
    let bound_lbl = gtk::Label::new(Some(bound_note));
    bound_lbl.set_halign(gtk::Align::Start);
    bound_lbl.set_xalign(0.0);
    bound_lbl.set_wrap(true);
    bound_lbl.add_css_class("caption");
    bound_lbl.add_css_class("dim-label");
    set_test_id(&bound_lbl, ids::NEST_TRUST_BACKUP_BOUND_NOTE);
    bi.append(&bound_lbl);

    let revoke_btn = gtk::Button::builder()
        .label(S::BACKUP_REVOKE)
        .css_classes(["destructive-action"])
        .build();
    set_test_id(&revoke_btn, ids::NEST_TRUST_BACKUP_REVOKE);
    // `revoke_btn` is a composite control: which of the two backup-revoke
    // ceremonies it binds is decided by `backup.kind`, fixed for this row's
    // whole lifetime (rebuilt fresh per snapshot — rule 4, declared once
    // rather than on a later re-paint).
    crate::offline_gate::declare_wire_kind(
        &revoke_btn,
        match backup.kind {
            TrustBackupKind::Seal => "fauna.backup.nest_key.revoke",
            TrustBackupKind::Writer => "fauna.backup.writer_grant.revoke",
        },
    );
    {
        let ctx = Rc::clone(ctx);
        let action = match backup.kind {
            TrustBackupKind::Seal => LinkedNestsAction::RevokeBackupSeal,
            TrustBackupKind::Writer => LinkedNestsAction::RevokeBackupWriter {
                destination_id: backup.destination_id.clone(),
            },
        };
        revoke_btn.connect_clicked(move |_| {
            dispatch_and_render(&ctx, action.clone(), false);
        });
    }
    revoke_btn.set_margin_top(2);
    bi.append(&revoke_btn);

    bi
}

/// `nest-trust-backup-status` label. Thin `resolve()` wrapper over the shared
/// `fauna_client_pair::backup_status_label` mapping (priority #1/#2).
fn backup_status_label(status: TrustBackupStatus) -> String {
    fauna_client_pair::backup_status_label(status).resolve(crate::i18n::strings::lookup)
}

/// Build one retained-generation row (`nest-trust-generation-item`) in the Now
/// lens on the home nest's row: what the owner can roll back to inside the
/// custody grace window `T` (`nests.md` § Trust facet — generation recovery).
/// Lifted from tui's landed `push_generation_item` — same shared row shape,
/// same three ratified honesty requirements:
///
/// - An `Unreachable` row renders **no** `nest-trust-generation-restore`
///   (`nests.md:122`) — there is no address to restore, and offering the
///   affordance would imply we knew something we do not. It is also why the
///   row exists at all: a destination we could not ask must never render as
///   "nothing to recover", the false reassurance a hostile source buys.
/// - A row with no plaintext `path` renders its **hash** rather than being
///   hidden or skipped (`nests.md:123`) — the rows a rogue source produced
///   are exactly the ones a user needs to see.
/// - The expiry leaf carries the REQUIRED quota-bound copy (`nests.md` §
///   Required copy): a user near their cap sees usage a supersede storm
///   inflated until `T`, and this is where that is explicable.
///
/// The three value leaves (superseded/expires/size) render EMPTY on an
/// unreachable row rather than a zero timestamp or "0 B", which would read as
/// fact — the elements stay registered so the row's leaf set does not vary by
/// status (the same shape `build_backup_item`'s seal-row `since` uses).
/// Ordering is the destination's, preserved by the shared projection — this
/// layer never re-sorts (`nests.md:117`).
fn build_generation_item(ctx: &Rc<Ctx>, generation: &TrustGenerationRow) -> gtk::Box {
    let gi = gtk::Box::new(gtk::Orientation::Vertical, 2);
    set_test_id(&gi, ids::NEST_TRUST_GENERATION_ITEM);
    gi.set_accessible_role(gtk::AccessibleRole::Group);
    gi.add_css_class("card");
    gi.set_margin_top(2);
    gi.set_margin_bottom(2);

    let unreachable = generation.status == TrustGenerationStatus::Unreachable;

    let path_lbl = field_label(
        "nest-trust-generation-path",
        &generation_path_text(generation),
    );
    path_lbl.add_css_class("heading");
    gi.append(&path_lbl);

    let superseded_text = if unreachable {
        String::new()
    } else {
        format!(
            "{} {}",
            S::GENERATION_SUPERSEDED,
            fauna_core::format::format_unix_local(generation.superseded_at)
        )
    };
    let superseded_lbl = field_label("nest-trust-generation-superseded", &superseded_text);
    superseded_lbl.add_css_class("caption");
    gi.append(&superseded_lbl);

    let expires_text = if unreachable {
        String::new()
    } else {
        S::generation_expires(&fauna_core::format::format_unix_local(
            generation.expires_at,
        ))
    };
    let expires_lbl = field_label("nest-trust-generation-expires", &expires_text);
    expires_lbl.add_css_class("caption");
    gi.append(&expires_lbl);

    let size_text = if unreachable {
        String::new()
    } else {
        crate::i18n::byte_size(generation.size_bytes.max(0) as u64)
    };
    let size_lbl = field_label("nest-trust-generation-size", &size_text);
    size_lbl.add_css_class("caption");
    gi.append(&size_lbl);

    let status_lbl = field_label(
        "nest-trust-generation-status",
        if unreachable {
            S::GENERATION_STATUS_UNREACHABLE
        } else {
            S::GENERATION_STATUS_LISTED
        },
    );
    status_lbl.add_css_class("caption");
    gi.append(&status_lbl);

    if generation_shows_restore(generation) {
        // The address triple round-trips off the row unchanged — never a row
        // index, which would promote the wrong generation the moment this
        // flattened list is filtered or re-ordered.
        let restore_btn = gtk::Button::builder()
            .label(S::GENERATION_RESTORE)
            .css_classes(["suggested-action"])
            .build();
        set_test_id(&restore_btn, ids::NEST_TRUST_GENERATION_RESTORE);
        crate::offline_gate::declare_wire_kind(&restore_btn, "fauna.backup.generation.restore");
        {
            let ctx = Rc::clone(ctx);
            let action = LinkedNestsAction::RestoreGeneration {
                destination_id: generation.destination_id.clone(),
                folder_name: generation.folder_name.clone(),
                path_hash: generation.path_hash.clone(),
                manifest_hash: generation.manifest_hash.clone(),
            };
            restore_btn.connect_clicked(move |_| {
                dispatch_and_render(&ctx, action.clone(), false);
            });
        }
        restore_btn.set_margin_top(2);
        gi.append(&restore_btn);
    }

    gi
}

/// The `nest-trust-generation-path` identity leaf (`nests.md:122`,`:123`).
///
/// **Ratified invariant, extracted so it is unit-testable without a live GTK
/// context** (`destinations.rs`'s `last_upload_text`/`backlog_text` idiom):
/// on an `Unreachable` row this names the DESTINATION that went dark, since
/// there is no generation to identify; on a `Listed` row with no plaintext
/// `path` (a sealed custody row with its path scrubbed), this renders
/// the `path_hash` rather than hiding or skipping the row — the rows a rogue
/// source produced are exactly the ones a user needs to see.
fn generation_path_text(generation: &TrustGenerationRow) -> String {
    if generation.status == TrustGenerationStatus::Unreachable {
        return generation.destination_label.clone();
    }
    match &generation.path {
        Some(path) => S::generation_path(path),
        None => S::generation_path_unknown(&generation.path_hash),
    }
}

/// Whether `nest-trust-generation-restore` renders for this row (`nests.md:122`).
///
/// **Ratified invariant, extracted so it is unit-testable without a live GTK
/// context.** An `Unreachable` row carries no restore address — offering the
/// affordance would imply we knew something we do not, and it is also why the
/// row exists at all: a destination we could not ask must never render as
/// "nothing to recover", the false reassurance a hostile source buys.
fn generation_shows_restore(generation: &TrustGenerationRow) -> bool {
    generation.status != TrustGenerationStatus::Unreachable
}

/// The `nest-trust-generation-notice` text for the page's last restore action
/// (`nests.md` § Trust facet — generation recovery, ratified 2026-07-29).
/// Never an error — `PastRecoveryWindow` is a product state, not a failure
/// (`nests.md:124`) — so this never rides `error_label`.
fn generation_notice_text(restore_outcome: Option<TrustRestoreOutcome>) -> &'static str {
    match restore_outcome {
        Some(TrustRestoreOutcome::Restored) => S::GENERATION_RESTORED,
        Some(TrustRestoreOutcome::PastRecoveryWindow) => S::GENERATION_PAST_WINDOW,
        None => "",
    }
}

/// Build one current-grant row (`nest-trust-grant-item`) for the Now lens: the
/// scope line ("Trusted to read: Mail, Calendar"), lasts-until, liveness status,
/// the REQUIRED honest-bound copy, per-grant renew/revoke, and — on a row the
/// succession aftermath raised — the review mark + Keep. `grant_id` round-trips
/// unchanged into every dispatch; `on_action` receives the action (tui's
/// `push_grant_item` names the row the same way), which keeps the row
/// buildable without a live page.
fn build_grant_item(
    grant: &TrustGrantRow,
    on_action: impl Fn(LinkedNestsAction) + Clone + 'static,
) -> gtk::Box {
    let gi = gtk::Box::new(gtk::Orientation::Vertical, 2);
    set_test_id(&gi, ids::NEST_TRUST_GRANT_ITEM);
    gi.set_accessible_role(gtk::AccessibleRole::Group);
    gi.add_css_class("card");
    gi.set_margin_top(2);
    gi.set_margin_bottom(2);

    let scope_lbl = field_label("nest-trust-grant-scope", &grant_scope_text(grant));
    scope_lbl.add_css_class("heading");
    gi.append(&scope_lbl);

    let lasts_text = format!(
        "{} {}",
        S::LASTS_UNTIL,
        fauna_core::format::format_unix_local(grant.lasts_until)
    );
    let lasts_lbl = field_label("nest-trust-grant-lasts-until", &lasts_text);
    lasts_lbl.add_css_class("caption");
    gi.append(&lasts_lbl);

    let status_lbl = field_label("nest-trust-grant-status", &status_label(grant.liveness));
    status_lbl.add_css_class("caption");
    gi.append(&status_lbl);

    // REQUIRED honest-bound copy (nests.md § Honest bound) — never over-promise.
    // A bounded (content-sealing-epochs) mail grant gets the stronger,
    // crypto-bounded wording WITH the honest INFO-A caveat; every other
    // kind/regime keeps the standing trust-until-revoke wording (flip-checklist
    // line 6). Never re-derive the (class, kind, tier) check here — the shared
    // predicate is the single source of truth (priority #2).
    let bound_note = if fauna_client_pair::trust_scope_is_bounded_mail_grant(grant.scope.clone()) {
        S::BOUND_NOTE_BOUNDED_MAIL
    } else {
        S::BOUND_NOTE_STANDING
    };
    let bound_lbl = gtk::Label::new(Some(bound_note));
    bound_lbl.set_halign(gtk::Align::Start);
    bound_lbl.set_xalign(0.0);
    bound_lbl.set_wrap(true);
    bound_lbl.add_css_class("caption");
    bound_lbl.add_css_class("dim-label");
    set_test_id(&bound_lbl, ids::NEST_TRUST_GRANT_BOUND_NOTE);
    gi.append(&bound_lbl);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    actions.set_margin_top(2);
    let renew_btn = gtk::Button::with_label(S::RENEW);
    set_test_id(&renew_btn, ids::NEST_TRUST_GRANT_RENEW);
    crate::offline_gate::declare_wire_kind(&renew_btn, "fauna.capabilities.renew");
    {
        let on_action = on_action.clone();
        let grant_id = grant.grant_id.clone();
        renew_btn.connect_clicked(move |_| {
            on_action(LinkedNestsAction::Renew {
                grant_id: grant_id.clone(),
            });
        });
    }
    let revoke_btn = gtk::Button::builder()
        .label(S::REVOKE)
        .css_classes(["destructive-action"])
        .build();
    set_test_id(&revoke_btn, ids::NEST_TRUST_GRANT_REVOKE);
    crate::offline_gate::declare_wire_kind(&revoke_btn, "fauna.capabilities.revoke");
    {
        let on_action = on_action.clone();
        let grant_id = grant.grant_id.clone();
        revoke_btn.connect_clicked(move |_| {
            on_action(LinkedNestsAction::Revoke {
                grant_id: grant_id.clone(),
            });
        });
    }
    actions.append(&renew_btn);
    actions.append(&revoke_btn);
    gi.append(&actions);

    // The post-succession review mark and its Keep half — present only while
    // this row is actually raised (`succession-aftermath.md` § Adjudicating what
    // the aftermath carries across; the shared `TrustGrantRow::unattested` is
    // the one reading of the mark plane, so nothing is re-derived here). Absent
    // rather than empty otherwise: in a healthy account every grant is the
    // owner's own, and a permanently-rendered mark would train the user straight
    // past the one succession that matters. Remove is deliberately NOT
    // re-rendered — `nest-trust-grant-revoke` above already is it, so Keep joins
    // the affordance that exists instead of minting a second revocation path.
    // tui's twin is `push_grant_item`.
    if grant.unattested {
        let mark = gtk::Label::new(Some(S::GRANT_UNATTESTED_MARK));
        mark.set_halign(gtk::Align::Start);
        mark.set_xalign(0.0);
        mark.set_wrap(true);
        mark.add_css_class("caption");
        set_test_id(&mark, ids::NEST_TRUST_GRANT_UNATTESTED_MARK);
        gi.append(&mark);

        let keep_btn = gtk::Button::with_label(S::GRANT_KEEP_BUTTON);
        keep_btn.set_halign(gtk::Align::Start);
        set_test_id(&keep_btn, ids::NEST_TRUST_GRANT_KEEP_BUTTON);
        // The verdict is an at-rest write to the owner's own account state.
        crate::offline_gate::declare_wire_kind(&keep_btn, "fauna.account.state.put");
        let grant_id = grant.grant_id.clone();
        keep_btn.connect_clicked(move |_| {
            on_action(LinkedNestsAction::KeepGrant {
                grant_id: grant_id.clone(),
            });
        });
        gi.append(&keep_btn);
    }
    gi
}

/// A grant row's `nest-trust-grant-scope` text ("Trusted to read: Mail").
fn grant_scope_text(grant: &TrustGrantRow) -> String {
    format!(
        "{} {}",
        S::TRUSTED_TO_READ,
        scope_line(&grant.scope, grant.folder.clone())
    )
}

/// A grant's scope line — the shared [`fauna_client_pair::grant_scope_labels`],
/// which names a folder grant's folder (`folder`, resolved in shared Rust)
/// in place of the bare folder read (priority #1/#2).
fn scope_line(scope: &[TrustScope], folder: Option<TrustFolder>) -> String {
    fauna_client_pair::grant_scope_labels(scope.to_vec(), folder)
        .into_iter()
        .map(|l| l.resolve(crate::i18n::strings::lookup))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Thin `resolve()` wrapper over the shared `fauna_client_pair::status_label`
/// mapping (priority #1/#2).
fn status_label(l: TrustLiveness) -> String {
    fauna_client_pair::status_label(l).resolve(crate::i18n::strings::lookup)
}

/// One History-lens row's self-describing line ("Trusted to read ‹scope› · ‹when›"
/// etc.) — the shared [`fauna_client_pair::history_line_text`] decision
/// (tui↔linux twin harvest, previously hand-rolled
/// identically here and on tui).
fn history_line(h: &TrustHistoryRow) -> String {
    let scope = scope_line(&h.scope, h.folder.clone());
    let when = fauna_core::format::format_unix_local(h.at);
    fauna_client_pair::history_line_text(h.kind, &scope, &when)
        .resolve(crate::i18n::strings::lookup)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_builds_without_client() {
        crate::testid::run_on_gtk_thread(|| {
            // No registered client → the page stays at static placeholders, but it
            // must still build with every ui.yaml ID present.
            let _page = build_linked_nests_page();
        });
    }

    /// The forward-queue block paints the count, the stuck hint, the reason and
    /// both actions — and a hostile relay-chosen reason arrives inert: the
    /// shared projection stripped its escape and newline, and the markup-off
    /// label shows `<b>x</b>` literally rather than as bold.
    #[test]
    fn forward_queue_block_paints_a_hostile_reason_inert() {
        crate::testid::run_on_gtk_thread(|| {
            let queue = ForwardQueueStatus::from(fauna_protocol::pair::ForwardQueue {
                queued: 2,
                stuck: 1,
                last_error: Some("\u{1b}[31m<b>x</b>\nforbidden".into()),
                ..Default::default()
            });
            let (block, _retry, _discard) = build_forward_queue(&queue);
            let mut labels = Vec::new();
            let mut child = block.first_child();
            while let Some(w) = child {
                if let Ok(l) = w.clone().downcast::<gtk::Label>() {
                    labels.push(l);
                }
                child = w.next_sibling();
            }
            assert_eq!(labels.len(), 2, "summary + reason");
            let summary = labels[0].text().to_string();
            assert!(summary.contains('2') && summary.contains('1'), "{summary}");
            let reason = &labels[1];
            assert!(!reason.uses_markup());
            let text = reason.text().to_string();
            assert!(text.contains("[31m<b>x</b>forbidden"), "{text}");
            assert!(!text.contains(['\u{1b}', '\n']), "{text:?}");
        });
    }

    fn custody_nest_row(pending: bool) -> CustodyRowView {
        use fauna_client_capabilities::view_model::ReceiptState;
        CustodyRowView {
            grant_id: vec![0x33; 16],
            host: [0xB2; 32],
            custodian_key: (!pending).then_some([0xC6; 32]),
            scopes: None,
            lasts_until: None,
            liveness: None,
            receipt: None,
            receipt_state: ReceiptState::NoReceiptYet,
            pending,
            custodian_nest_url: Some("wss://friend.example".into()),
        }
    }

    fn label_text(root: &gtk::Box, id: &str) -> String {
        crate::testid::find_by_test_id(root, id)
            .unwrap_or_else(|| panic!("{id} renders"))
            .downcast::<gtk::Label>()
            .expect("a label")
            .text()
            .to_string()
    }

    /// A nest-anchored custody renders as its own `nests-item` with the full
    /// `nest-trust-custody-*` family (tui's `push_custody_nest_item`): the
    /// trust-vocabulary scope, the no-receipt-yet state (never empty), held
    /// bytes, and a revoke live exactly when the custody is minted.
    #[test]
    fn a_custodian_nest_renders_the_custody_family() {
        crate::testid::run_on_gtk_thread(|| {
            use crate::i18n::strings::devices as D;
            // The revoke is offline-gated; start from a connected gate so the
            // assertions below read the page's own intent (`reset_for_test`'s
            // reason — one GTK thread shared by every widget test).
            crate::offline_gate::reset_for_test("connected");
            let item = build_custody_nest_item(&custody_nest_row(false), |_, _| {});
            assert!(
                label_text(&item, ids::NESTS_ITEM_LABEL).contains(&S::custody_nest_label(
                    &fauna_core::format::short_id(&hex::encode([0xB2; 32]))
                ))
            );
            assert!(crate::testid::find_by_test_id(&item, ids::NEST_TRUST_CUSTODY_ITEM).is_some());
            assert_eq!(
                label_text(&item, ids::NEST_TRUST_CUSTODY_SCOPE),
                D::CUSTODY_HOLDER_SCOPE
            );
            assert_eq!(
                label_text(&item, ids::NEST_TRUST_CUSTODY_RECEIPT_STATUS),
                crate::i18n::custody_receipt_status(
                    fauna_client_capabilities::view_model::ReceiptState::NoReceiptYet,
                    None
                )
            );
            assert!(!label_text(&item, ids::NEST_TRUST_CUSTODY_HELD_BYTES).is_empty());
            let revoke =
                crate::testid::find_by_test_id(&item, ids::NEST_TRUST_CUSTODY_REVOKE_BUTTON)
                    .expect("revoke renders");
            assert!(revoke.is_sensitive());

            let pending = build_custody_nest_item(&custody_nest_row(true), |_, _| {});
            let revoke =
                crate::testid::find_by_test_id(&pending, ids::NEST_TRUST_CUSTODY_REVOKE_BUTTON)
                    .expect("revoke renders");
            assert!(
                !revoke.is_sensitive(),
                "a pending ceremony minted nothing to revoke"
            );
        });
    }

    /// The web-serve paywall grant names its folder — on the grant row and on
    /// its History `Revoke`, which carries no scope (`nests.md` § Trust facet
    /// — grants; the folder resolved in shared Rust). tui's twin is
    /// `a_paywall_grant_and_its_revoke_name_the_folder`.
    #[test]
    fn a_paywall_grant_and_its_revoke_name_the_folder() {
        let premium = Some(TrustFolder::Named {
            name: "premium".into(),
        });
        let grant = TrustGrantRow {
            grant_id: vec![7; 16],
            holder: vec![9; 32],
            scope: vec![TrustScope {
                class: "content.read".into(),
                kind: Some("folder".into()),
                tier: None,
            }],
            lasts_until: 1_700_000_000,
            liveness: TrustLiveness::Active,
            unattested: false,
            folder: premium.clone(),
        };
        assert_eq!(
            grant_scope_text(&grant),
            r#"Trusted to read: Your folder "premium""#
        );

        let revoke = TrustHistoryRow {
            grant_id: vec![6; 16],
            holder: vec![9; 32],
            kind: fauna_client_pair::TrustEventKind::Revoke,
            scope: Vec::new(),
            window_start: 0,
            window_end: 0,
            at: 1_700_000_000,
            folder: premium,
        };
        let line = history_line(&revoke);
        assert!(
            line.starts_with(r#"Trust revoked: Your folder "premium" · "#),
            "a Revoke names the folder its grant covered, got {line:?}"
        );
    }

    fn grant_row(unattested: bool) -> TrustGrantRow {
        TrustGrantRow {
            grant_id: vec![7; 16],
            holder: vec![9; 32],
            scope: vec![TrustScope {
                class: "content.read".into(),
                kind: Some("mail".into()),
                tier: None,
            }],
            lasts_until: 1_700_000_000,
            liveness: TrustLiveness::Active,
            unattested,
            folder: None,
        }
    }

    /// The post-succession review pair renders only on a raised row, and Keep
    /// dispatches `KeepGrant` naming the row it painted on
    /// (`succession-aftermath.md` § Adjudicating what the aftermath carries
    /// across). tui's twins are the `push_grant_item` render pins.
    #[test]
    fn a_raised_grant_carries_the_review_mark_and_a_keep_naming_it() {
        crate::testid::run_on_gtk_thread(|| {
            crate::offline_gate::reset_for_test("connected");
            let clean = build_grant_item(&grant_row(false), |_| {});
            assert!(
                crate::testid::find_by_test_id(&clean, ids::NEST_TRUST_GRANT_UNATTESTED_MARK)
                    .is_none(),
                "an ordinary grant carries no mark"
            );
            assert!(
                crate::testid::find_by_test_id(&clean, ids::NEST_TRUST_GRANT_KEEP_BUTTON).is_none()
            );

            let seen: Rc<RefCell<Vec<Vec<u8>>>> = Rc::default();
            let sink = Rc::clone(&seen);
            let raised = build_grant_item(&grant_row(true), move |a| match a {
                LinkedNestsAction::KeepGrant { grant_id } => sink.borrow_mut().push(grant_id),
                _ => panic!("Keep must dispatch KeepGrant"),
            });
            assert_eq!(
                label_text(&raised, ids::NEST_TRUST_GRANT_UNATTESTED_MARK),
                S::GRANT_UNATTESTED_MARK
            );
            let keep = crate::testid::find_by_test_id(&raised, ids::NEST_TRUST_GRANT_KEEP_BUTTON)
                .expect("Keep renders on a raised row")
                .downcast::<gtk::Button>()
                .expect("a button");
            keep.emit_clicked();
            assert_eq!(
                seen.borrow().as_slice(),
                &[vec![7u8; 16]],
                "Keep must name its own row, once"
            );
        });
    }

    fn home_row(blessed: bool, default: fauna_client_pair::TrustGrantDuration) -> LinkedNestRow {
        LinkedNestRow {
            nest_id: "aa".repeat(32),
            capabilities: Vec::new(),
            capability_labels: Vec::new(),
            expires_at: None,
            created_at: 0,
            label: None,
            nest_url: None,
            is_home: true,
            trust_grants: Vec::new(),
            trust_history: Vec::new(),
            lens: TrustLens::Now,
            available_holders: Vec::new(),
            mint_options: Vec::new(),
            blessed,
            mint_default_duration: default,
            trust_backups: Vec::new(),
            trust_generations: Vec::new(),
        }
    }

    /// The duration select offers the shared options and pre-selects the
    /// row's default — hours on an un-blessed nest, the standing window on a
    /// blessed one — and reads back whatever the owner picks.
    #[test]
    fn duration_select_preselects_the_rows_default() {
        use fauna_client_pair::TrustGrantDuration as D;
        crate::testid::run_on_gtk_thread(|| {
            for default in [D::OneOff, D::Standard] {
                let dd = build_duration_select(&home_row(false, default));
                assert_eq!(selected_duration(&dd, D::OneOff), default);
                let model = dd.model().expect("a model");
                assert_eq!(
                    model.n_items() as usize,
                    fauna_client_pair::mint_duration_options().len()
                );
            }
            let dd = build_duration_select(&home_row(false, D::OneOff));
            let standard = fauna_client_pair::mint_duration_options()
                .iter()
                .position(|d| *d == D::Standard)
                .expect("standard is offered");
            dd.set_selected(standard as u32);
            assert_eq!(selected_duration(&dd, D::OneOff), D::Standard);
        });
    }

    /// The escrow-holder badge's predicate: exactly the nest row whose identity
    /// is among the recorded escrow holders.
    #[test]
    fn escrow_badge_matches_the_holder_row_only() {
        let holders = [[0xAA; 32]];
        assert!(holds_escrow(&hex::encode([0xAA; 32]), &holders));
        assert!(!holds_escrow(&hex::encode([0xBB; 32]), &holders));
        assert!(!holds_escrow("not-hex", &holders));
    }

    #[test]
    fn forward_queue_block_omits_the_reason_until_a_send_failed() {
        crate::testid::run_on_gtk_thread(|| {
            let queue = ForwardQueueStatus {
                queued: 1,
                stuck: 0,
                last_error: None,
            };
            let (block, _retry, _discard) = build_forward_queue(&queue);
            let mut count = 0;
            let mut child = block.first_child();
            while let Some(w) = child {
                if w.is::<gtk::Label>() {
                    count += 1;
                }
                child = w.next_sibling();
            }
            assert_eq!(count, 1, "summary only — never an empty reason line");
        });
    }

    fn generation_row(status: TrustGenerationStatus, path: Option<&str>) -> TrustGenerationRow {
        TrustGenerationRow {
            status,
            destination_id: "dest-1".to_string(),
            destination_label: "Recovery".to_string(),
            folder_name: "__mail".to_string(),
            path: path.map(str::to_string),
            path_hash: "aa11".to_string(),
            manifest_hash: "mm22".to_string(),
            size_bytes: 2048,
            superseded_at: 1_700_000_000,
            expires_at: 1_702_592_000,
        }
    }

    // Mutation 1 (nests.md:122): an Unreachable row must never offer the
    // restore affordance — there is no address to restore, and offering it
    // would imply we knew something we do not.
    #[test]
    fn unreachable_row_offers_no_restore() {
        let row = generation_row(TrustGenerationStatus::Unreachable, None);
        assert!(!generation_shows_restore(&row));
    }

    #[test]
    fn listed_row_offers_restore() {
        let row = generation_row(TrustGenerationStatus::Listed, Some("/Mail/2026"));
        assert!(generation_shows_restore(&row));
    }

    // Mutation 2 (nests.md:123): a path-less Listed row (a sealed custody
    // row with its path scrubbed) must render its hash, never be hidden or skipped
    // — the rows a rogue source produced are exactly the ones a user needs to
    // see.
    #[test]
    fn path_less_listed_row_renders_the_hash_not_hidden() {
        let row = generation_row(TrustGenerationStatus::Listed, None);
        let text = generation_path_text(&row);
        assert!(
            !text.is_empty(),
            "a path-less row must still render identity text"
        );
        assert!(
            text.contains(&row.path_hash),
            "no plaintext path ⇒ the hash fallback, got {text:?}"
        );
    }

    #[test]
    fn listed_row_with_path_renders_the_plaintext_path() {
        let row = generation_row(TrustGenerationStatus::Listed, Some("/Mail/2026"));
        assert!(generation_path_text(&row).contains("/Mail/2026"));
    }

    // An unreachable row's identity leaf names the DESTINATION that went dark,
    // not a generation that does not exist.
    #[test]
    fn unreachable_row_path_text_names_the_destination() {
        let row = generation_row(TrustGenerationStatus::Unreachable, None);
        assert_eq!(generation_path_text(&row), row.destination_label);
    }

    // ── restore-outcome notice (`nest-trust-generation-notice`, ratified
    // 2026-07-29) ──────────────────────────────────────────────────────────

    #[test]
    fn no_restore_outcome_renders_an_empty_notice() {
        assert_eq!(generation_notice_text(None), "");
    }

    #[test]
    fn a_restored_outcome_renders_its_own_notice_text() {
        assert_eq!(
            generation_notice_text(Some(TrustRestoreOutcome::Restored)),
            S::GENERATION_RESTORED
        );
    }

    #[test]
    fn a_past_recovery_window_outcome_renders_its_own_notice_text_never_failed() {
        let text = generation_notice_text(Some(TrustRestoreOutcome::PastRecoveryWindow));
        assert_eq!(text, S::GENERATION_PAST_WINDOW);
        assert!(
            !text.to_lowercase().contains("failed"),
            "past-the-window is a product state, never a failure message (nests.md:124)"
        );
    }
}
