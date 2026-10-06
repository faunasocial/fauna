//! The user-facing "List members" preferences page (linux; the mail-UX seed's
//! lead app).
//!
//! Where a person manages the members of **one** of their mailing lists: the
//! subscribed/unsubscribed summary, add a member, batch-import addresses, and
//! per-member unsubscribe / resubscribe. Target behavior:
//! `docs/goal/behavior/mail-mass-mailing.md` § `mail-list-members` page. UX/IDs:
//! `tests/e2e-unified/ui.yaml` `mail-list-members` page + `mail-list-members-list`
//! component.
//!
//! Dumb renderer of [`MailListMembersSnapshot`] + dispatcher of
//! [`MailListMembersAction`]; all logic lives in the shared
//! `fauna_client_mail_settings::lists` machine (priority #2/#4). Direct sibling:
//! `settings/mail_lists.rs`.
//!
//! # UI precedes backend (surfaced, never faked) — backend landed 2026-06-13/14
//!
//! The shared `MailListMembersNest` seam calls the real `fauna.bridges.*_list_*`
//! RPCs (`mail-mass-mailing.md` § Implementation status today); a `fake`/
//! disconnected nest still renders an honest `error-message` rather than
//! fabricating member rows.
//!
//! Built once into the settings stack with [`PLACEHOLDER_LIST_ID_HEX`]; the
//! `select` closure this module returns re-targets it at the list whose
//! `mail-lists` "Members" button was actually clicked (`mail_lists.rs`'s
//! `on_navigate_to_members`, composed in `settings_shell`).

use fauna_ui_ids as ids;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;

use fauna_client_mail_settings::{
    MailListMembersAction, MailListMembersMachine, MailListMembersSnapshot, MemberStatus,
    MemberView,
};

use crate::async_helper::spawn_with_snapshot;
use crate::client::FaunaClient;
use crate::i18n::strings::mail_lists as S;
use crate::testid::set_test_id;

/// Placeholder list id for the embedded seed (the full app passes the real id
/// from the opened `mail-lists` row).
const PLACEHOLDER_LIST_ID_HEX: &str = "00000000000000000000000000000000";

#[derive(Clone)]
struct MembersWidgets {
    error_label: gtk::Label,
    summary: gtk::Label,
    add_button: gtk::Button,
    import_button: gtk::Button,
    list_group: adw::PreferencesGroup,
    list_placeholder: adw::ActionRow,
    rows: Rc<RefCell<Vec<adw::ActionRow>>>,
    // Add-member sheet.
    add_sheet: adw::PreferencesGroup,
    address_input: gtk::Entry,
    add_submit: gtk::Button,
    add_cancel: gtk::Button,
    // Import sheet.
    import_sheet: adw::PreferencesGroup,
    import_input: gtk::TextView,
    import_submit: gtk::Button,
    import_cancel: gtk::Button,
}

struct MembersCtx {
    // `RefCell` because [`select`] swaps in a freshly-built machine scoped to
    // whichever list `mail-lists`' Members button was clicked for — the page
    // itself is built once (into the shared settings stack), but the list it
    // shows is re-targeted per navigation, mirroring tui's
    // `MailListMembersState::select`.
    machine: RefCell<Arc<MailListMembersMachine>>,
    client: Rc<FaunaClient>,
    rt: tokio::runtime::Handle,
    w: MembersWidgets,
    /// Whether `select()` has ever retargeted the page at a real list, vs.
    /// still sitting on the built-in `PLACEHOLDER_LIST_ID_HEX`. Mirrors tui's
    /// `has_list = m.machine.is_some()` (`apps/fauna-tui/src/settings/mail_list_members.rs`)
    /// — drives which of the two rule-5 reasons `render()` paints.
    has_list: Cell<bool>,
}

/// `select(list_id_hex, friendly_name)` — `mail-lists`' Members button calls it
/// (via `settings_shell`, which also switches the visible stack child) to
/// re-target this ALREADY-BUILT page at a different list, rebuilding its
/// machine and re-hydrating. Mirrors tui's `MailListMembersState::select`.
type SelectClosure = Rc<dyn Fn(String, String)>;
/// The direct-rail-visit fallback resolver — see [`resolve_fallback`].
type ResolveFallbackClosure = Rc<dyn Fn()>;

/// Build the "List members" preferences page. Returns the page plus its two
/// navigation hooks: [`SelectClosure`] and [`ResolveFallbackClosure`].
pub fn build_mail_list_members_page()
-> (adw::PreferencesPage, SelectClosure, ResolveFallbackClosure) {
    let page = adw::PreferencesPage::builder()
        .title(S::MEMBERS_TITLE)
        .icon_name("system-users-symbolic")
        .build();

    let top_group = adw::PreferencesGroup::builder()
        .title(S::MEMBERS_TITLE)
        .build();
    top_group.set_header_suffix(Some(&super::marker("page-heading")));
    let error_label = gtk::Label::builder().visible(false).build();
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    let error_row = adw::ActionRow::builder().activatable(false).build();
    error_row.add_suffix(&error_label);
    top_group.add(&error_row);

    // mail-list-members-summary — subscribed / unsubscribed counts.
    let summary = super::value_marker("mail-list-members-summary", "");
    let summary_row = adw::ActionRow::builder().title(S::MEMBERS_TITLE).build();
    summary_row.add_suffix(&summary);
    top_group.add(&summary_row);

    let add_button = gtk::Button::builder()
        .label(S::ADD_MEMBER_BUTTON)
        .valign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .build();
    set_test_id(&add_button, ids::MAIL_LIST_MEMBERS_ADD_BUTTON);
    let import_button = gtk::Button::builder()
        .label(S::IMPORT_BUTTON)
        .valign(gtk::Align::Center)
        .build();
    set_test_id(&import_button, ids::MAIL_LIST_MEMBERS_IMPORT_BUTTON);
    let buttons_row = adw::ActionRow::builder().activatable(false).build();
    buttons_row.add_suffix(&add_button);
    buttons_row.add_suffix(&import_button);
    top_group.add(&buttons_row);
    page.add(&top_group);

    let list_group = adw::PreferencesGroup::builder().build();
    list_group.set_header_suffix(Some(&super::marker("mail-list-members-list")));
    // The page's two own reasons (`ui/README.md` rule 5) — it used to borrow
    // the *Lists* page's `EMPTY` ("No lists yet"), which under a "Members"
    // heading read as a wrong claim about this list's membership. Starts on
    // `MEMBERS_NO_LIST` — accurate for the built-but-not-yet-`select()`'d
    // state (`has_list` starts false); `select()` and `render()` below drive
    // it from there.
    let list_placeholder = adw::ActionRow::builder().title(S::MEMBERS_NO_LIST).build();
    list_group.add(&list_placeholder);
    page.add(&list_group);

    // --- Add-member sheet (hidden) ---
    let add_sheet = adw::PreferencesGroup::builder()
        .title(S::ADD_MEMBER_BUTTON)
        .build();
    add_sheet.set_visible(false);
    let add_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let address_input = gtk::Entry::builder()
        .placeholder_text(S::ADD_MEMBER_PLACEHOLDER)
        .build();
    set_test_id(
        &address_input,
        ids::MAIL_LIST_MEMBERS_ADD_SHEET_ADDRESS_INPUT,
    );
    add_box.append(&address_input);
    let add_submit = gtk::Button::builder()
        .label(S::ADD_MEMBER_SUBMIT)
        .css_classes(["suggested-action"])
        .build();
    set_test_id(&add_submit, ids::MAIL_LIST_MEMBERS_ADD_SHEET_SUBMIT_BUTTON);
    crate::offline_gate::declare_wire_kind(&add_submit, "fauna.bridges.add_list_member");
    add_box.append(&add_submit);
    let add_cancel = gtk::Button::builder().label(S::ADD_MEMBER_CANCEL).build();
    set_test_id(&add_cancel, ids::MAIL_LIST_MEMBERS_ADD_SHEET_CANCEL_BUTTON);
    add_box.append(&add_cancel);
    add_sheet.add(&add_box);
    page.add(&add_sheet);

    // --- Import sheet (hidden) ---
    // `mail_lists.import_placeholder` ("One email address per line") — the
    // other apps render it as the input's placeholder; GtkTextView has no
    // native placeholder, so the group description is the idiomatic GTK hint slot.
    let import_sheet = adw::PreferencesGroup::builder()
        .title(S::IMPORT_BUTTON)
        .description(S::IMPORT_PLACEHOLDER)
        .build();
    import_sheet.set_visible(false);
    let import_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    // mail-list-members-import-sheet-input — multi-line (one address per line).
    let import_input = gtk::TextView::new();
    import_input.set_height_request(120);
    import_input.set_monospace(true);
    set_test_id(&import_input, ids::MAIL_LIST_MEMBERS_IMPORT_SHEET_INPUT);
    let import_scroll = gtk::ScrolledWindow::builder().child(&import_input).build();
    import_box.append(&import_scroll);
    let import_submit = gtk::Button::builder()
        .label(S::IMPORT_SUBMIT)
        .css_classes(["suggested-action"])
        .build();
    set_test_id(
        &import_submit,
        ids::MAIL_LIST_MEMBERS_IMPORT_SHEET_SUBMIT_BUTTON,
    );
    crate::offline_gate::declare_wire_kind(
        &import_submit,
        "fauna.bridges.batch_import_list_members",
    );
    import_box.append(&import_submit);
    let import_cancel = gtk::Button::builder().label(S::IMPORT_CANCEL).build();
    set_test_id(
        &import_cancel,
        ids::MAIL_LIST_MEMBERS_IMPORT_SHEET_CANCEL_BUTTON,
    );
    import_box.append(&import_cancel);
    import_sheet.add(&import_box);
    page.add(&import_sheet);

    let widgets = MembersWidgets {
        error_label,
        summary,
        add_button,
        import_button,
        list_group,
        list_placeholder,
        rows: Rc::new(RefCell::new(Vec::new())),
        add_sheet,
        address_input,
        add_submit,
        add_cancel,
        import_sheet,
        import_input,
        import_submit,
        import_cancel,
    };
    let (select, resolve_fallback) = wire_machine(widgets);

    (page, select, resolve_fallback)
}

fn wire_machine(widgets: MembersWidgets) -> (SelectClosure, ResolveFallbackClosure) {
    let client = match crate::settings::get_client() {
        Some(c) => c,
        // No client (the unit-test path, or a not-yet-signed-in shell): the
        // page renders its static ID scaffold only, so selecting a list is a
        // no-op rather than a panic.
        None => return (Rc::new(|_, _| {}), Rc::new(|| {})),
    };
    let machine = match crate::mail_glue::build_mail_list_members_machine(
        &client,
        PLACEHOLDER_LIST_ID_HEX.to_string(),
        S::MEMBERS_TITLE.to_string(),
    ) {
        Ok(m) => Arc::new(m),
        Err(_) => return (Rc::new(|_, _| {}), Rc::new(|| {})),
    };

    let ctx = Rc::new(MembersCtx {
        machine: RefCell::new(machine),
        rt: client.runtime_handle(),
        client,
        w: widgets,
        has_list: Cell::new(false),
    });

    hydrate_and_render(&ctx);

    {
        let ctx = Rc::clone(&ctx);
        ctx.w.add_button.clone().connect_clicked(move |_| {
            ctx.w.import_sheet.set_visible(false);
            ctx.w.address_input.set_text("");
            ctx.w.add_sheet.set_visible(true);
        });
    }
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .add_cancel
            .clone()
            .connect_clicked(move |_| ctx.w.add_sheet.set_visible(false));
    }
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.add_submit.clone().connect_clicked(move |_| {
            let address = ctx.w.address_input.text().trim().to_string();
            if !address.is_empty() {
                dispatch_then_close(
                    &ctx,
                    MailListMembersAction::AddMember { address },
                    CloseSheet::Add,
                );
            }
        });
    }
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.import_button.clone().connect_clicked(move |_| {
            ctx.w.add_sheet.set_visible(false);
            ctx.w.import_input.buffer().set_text("");
            ctx.w.import_sheet.set_visible(true);
        });
    }
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .import_cancel
            .clone()
            .connect_clicked(move |_| ctx.w.import_sheet.set_visible(false));
    }
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.import_submit.clone().connect_clicked(move |_| {
            let buf = ctx.w.import_input.buffer();
            let (start, end) = buf.bounds();
            let addresses = buf.text(&start, &end, false).to_string();
            dispatch_then_close(
                &ctx,
                MailListMembersAction::BatchImport { addresses },
                CloseSheet::Import,
            );
        });
    }

    let select_closure: SelectClosure = {
        let ctx = Rc::clone(&ctx);
        Rc::new(move |list_id_hex: String, friendly_name: String| {
            select(&ctx, list_id_hex, friendly_name);
        })
    };
    let resolve_fallback_closure: ResolveFallbackClosure = {
        let ctx = Rc::clone(&ctx);
        Rc::new(move || resolve_fallback(&ctx))
    };
    (select_closure, resolve_fallback_closure)
}

/// A direct rail visit to this page before any row's Members button has ever
/// been clicked here (`has_list` still false): mirrors windows'/apple's shape
/// (mail-mass-mailing.md § Per-app render status — the tui shape, i.e. "nothing
/// selected -> fall back to the caller's first owned list") rather than tui's
/// own — this page has no already-hydrated `MailListsMachine` to peek at (each
/// settings page here is built independently, `settings_shell`'s composition),
/// so it builds a throwaway one, takes the first list, and falls back to it via
/// [`select`]; the placeholder empty state stays reserved for the genuinely-
/// zero-lists case. Wired to the settings shell's stack visible-child-changed
/// signal (`settings_shell.rs`), which also fires on the row-click path — but
/// `select` there always runs FIRST and sets `has_list`, so this is a no-op
/// then, not a race.
fn resolve_fallback(ctx: &Rc<MembersCtx>) {
    if ctx.has_list.get() {
        return;
    }
    let lists_machine = crate::mail_glue::build_mail_lists_machine(&ctx.client);
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            let _ = lists_machine.hydrate().await;
            lists_machine.snapshot()
        },
        move |snap| {
            // A row's Members click cannot land while this hydrate is in
            // flight (both run on the same GTK main loop thread), but a
            // second `resolve_fallback` from a rapid re-visit could already
            // have resolved by the time this one's hydrate returns.
            if ctx_render.has_list.get() {
                return;
            }
            if let Some(first) = snap.lists.first() {
                select(
                    &ctx_render,
                    first.list_id_hex.clone(),
                    first.friendly_name.clone(),
                );
            }
        },
    );
}

/// Re-target an already-built members page at a different list: rebuild the
/// machine scoped to `list_id_hex`, swap it into `ctx.machine`, and re-hydrate.
/// A rebuild failure leaves the previous machine in place rather than panic —
/// the next successful selection or manual refresh recovers.
fn select(ctx: &Rc<MembersCtx>, list_id_hex: String, friendly_name: String) {
    match crate::mail_glue::build_mail_list_members_machine(&ctx.client, list_id_hex, friendly_name)
    {
        Ok(m) => *ctx.machine.borrow_mut() = Arc::new(m),
        Err(_) => return,
    }
    ctx.has_list.set(true);
    // Immediate feedback for the real loading window: a list IS open now,
    // but its snapshot hasn't landed yet — that is `members_loading`, not
    // `members_no_list` (`ui/README.md` rule 5's pre-hydrate case).
    ctx.w.list_placeholder.set_title(S::MEMBERS_LOADING);
    ctx.w.list_placeholder.set_visible(true);
    hydrate_and_render(ctx);
}

#[derive(Clone, Copy)]
enum CloseSheet {
    Add,
    Import,
}

fn hydrate_and_render(ctx: &Rc<MembersCtx>) {
    let machine = ctx.machine.borrow().clone();
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            let _ = machine.hydrate().await;
            machine.snapshot()
        },
        move |snap| render(&ctx_render, &snap),
    );
}

fn dispatch_action(ctx: &Rc<MembersCtx>, action: MailListMembersAction) {
    let machine = ctx.machine.borrow().clone();
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            let _ = machine.dispatch(action).await;
            machine.snapshot()
        },
        move |snap| render(&ctx_render, &snap),
    );
}

fn dispatch_then_close(ctx: &Rc<MembersCtx>, action: MailListMembersAction, close: CloseSheet) {
    let machine = ctx.machine.borrow().clone();
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            let _ = machine.dispatch(action).await;
            machine.snapshot()
        },
        move |snap| {
            render(&ctx_render, &snap);
            if snap.error.is_none() {
                match close {
                    CloseSheet::Add => ctx_render.w.add_sheet.set_visible(false),
                    CloseSheet::Import => ctx_render.w.import_sheet.set_visible(false),
                }
            }
        },
    );
}

fn render(ctx: &Rc<MembersCtx>, snap: &MailListMembersSnapshot) {
    let w = &ctx.w;

    super::render_error_label(&w.error_label, snap.error.as_deref());

    w.summary.set_text(&S::summary_fmt(
        &snap.subscribed_count.to_string(),
        &snap.unsubscribed_count.to_string(),
    ));

    {
        let mut rows = w.rows.borrow_mut();
        for row in rows.drain(..) {
            w.list_group.remove(&row);
        }
        for view in &snap.members {
            let row = build_member_row(ctx, view);
            w.list_group.add(&row);
            rows.push(row);
        }
    }
    // A real list's snapshot has landed — Add/Import are enabled by now
    // (`has_list`), so a genuinely empty member list needs no rule-5 reason:
    // the "0 subscribed · 0 unsubscribed" summary above already says it,
    // matching tui's reference (no chrome once `has_list && snapshot present`,
    // regardless of member count).
    w.list_placeholder.set_visible(!ctx.has_list.get());
}

fn build_member_row(ctx: &Rc<MembersCtx>, view: &MemberView) -> adw::ActionRow {
    let status = fauna_client_mail_settings::member_status_label(view.status)
        .resolve(crate::i18n::strings::lookup);
    let row = adw::ActionRow::builder()
        .title(&view.address)
        .subtitle(&status)
        .build();
    row.add_prefix(&super::marker("mail-list-members-list-item"));

    row.add_suffix(&super::value_marker(
        "mail-list-members-list-item-address",
        &view.address,
    ));
    row.add_suffix(&super::value_marker(
        "mail-list-members-list-item-subscribed-at",
        &view
            .subscribed_at_ms
            .map(crate::i18n::local_date)
            .unwrap_or_default(),
    ));
    row.add_suffix(&super::value_marker(
        "mail-list-members-list-item-status",
        &status,
    ));

    // Unsubscribe (active when subscribed) / Resubscribe (active when not).
    let unsubscribe_button = gtk::Button::builder()
        .label(S::UNSUBSCRIBE)
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .sensitive(view.status == MemberStatus::Subscribed)
        .build();
    set_test_id(
        &unsubscribe_button,
        ids::MAIL_LIST_MEMBERS_LIST_ITEM_UNSUBSCRIBE_BUTTON,
    );
    crate::offline_gate::declare_wire_kind(
        &unsubscribe_button,
        "fauna.bridges.unsubscribe_list_member",
    );
    {
        let ctx = Rc::clone(ctx);
        let address = view.address.clone();
        unsubscribe_button.connect_clicked(move |_| {
            dispatch_action(
                &ctx,
                MailListMembersAction::Unsubscribe {
                    address: address.clone(),
                },
            );
        });
    }
    row.add_suffix(&unsubscribe_button);

    let resubscribe_button = gtk::Button::builder()
        .label(S::RESUBSCRIBE)
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .sensitive(view.status == MemberStatus::Unsubscribed)
        .build();
    set_test_id(
        &resubscribe_button,
        ids::MAIL_LIST_MEMBERS_LIST_ITEM_RESUBSCRIBE_BUTTON,
    );
    crate::offline_gate::declare_wire_kind(
        &resubscribe_button,
        "fauna.bridges.resubscribe_list_member",
    );
    {
        let ctx = Rc::clone(ctx);
        let address = view.address.clone();
        resubscribe_button.connect_clicked(move |_| {
            dispatch_action(
                &ctx,
                MailListMembersAction::Resubscribe {
                    address: address.clone(),
                },
            );
        });
    }
    row.add_suffix(&resubscribe_button);

    row
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::testid::widget_names;

    #[test]
    fn members_page_exposes_static_ui_yaml_ids() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let (page, _select, _resolve_fallback) = build_mail_list_members_page();
            let names = widget_names(&page);
            for id in [
                "page-heading",
                "error-message",
                "mail-list-members-summary",
                "mail-list-members-add-button",
                "mail-list-members-add-sheet-address-input",
                "mail-list-members-add-sheet-submit-button",
                "mail-list-members-add-sheet-cancel-button",
                "mail-list-members-import-button",
                "mail-list-members-import-sheet-input",
                "mail-list-members-import-sheet-submit-button",
                "mail-list-members-import-sheet-cancel-button",
                "mail-list-members-list",
            ] {
                assert!(
                    names.iter().any(|n| n == id),
                    "missing widget id {id:?}; have {names:?}"
                );
            }
        });
    }
}
