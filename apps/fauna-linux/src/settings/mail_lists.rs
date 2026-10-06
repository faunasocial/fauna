//! The user-facing "Lists" preferences page (linux; the mail-UX seed's lead
//! app).
//!
//! Where a person runs **their own** mailing lists (a list is a sixth alias
//! kind): create / edit / delete a list, each with a friendly name, a send-from
//! address on one of the user's domains, optional List-Help / List-Archive URLs,
//! and a per-send recipient cap. Target behavior:
//! `docs/goal/behavior/mail-mass-mailing.md` § `mail-lists` page UX. UX/IDs:
//! `tests/e2e-unified/ui.yaml` `mail-lists` page + `mail-lists-list` component.
//!
//! Per `mail-mass-mailing.md` § Architectural rules this layer holds **no**
//! business logic — it is a dumb renderer of [`MailListsSnapshot`] + dispatcher
//! of [`MailListsAction`]; the projection + action sequencing live in the shared
//! `fauna_client_mail_settings::lists` machine (priority #2/#4), the prior art the
//! other five apps lift. Direct sibling: `settings/mail_aliases.rs`.
//!
//! # UI precedes backend (surfaced, never faked) — backend landed 2026-06-13/14
//!
//! The shared `MailListsNest` seam calls the real `fauna.bridges.*_list_*`
//! RPCs (`mail-mass-mailing.md` § Implementation status today); a `fake`/
//! disconnected nest still renders an honest `error-message` rather than
//! fabricating list rows.
//!
//! The per-row "Members" button scopes the `mail-list-members` page to that
//! row's list and routes to it (`on_navigate_to_members`, wired by
//! `settings_shell` — mirrors tui's `Action::MailListsOpenMembers`).

use fauna_ui_ids as ids;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;

use fauna_client_mail_settings::{
    ListDraft, ListView, MailListsAction, MailListsMachine, MailListsSnapshot,
};

use crate::async_helper::spawn_with_snapshot;
use crate::i18n::strings::mail_lists as S;
use crate::testid::set_test_id;

#[derive(Clone, Copy, PartialEq, Eq)]
enum FormMode {
    Add,
    Edit,
}

#[derive(Clone)]
struct ListsWidgets {
    error_label: gtk::Label,
    add_button: gtk::Button,
    list_group: adw::PreferencesGroup,
    list_placeholder: adw::ActionRow,
    rows: Rc<RefCell<Vec<adw::ActionRow>>>,
    sheet_group: adw::PreferencesGroup,
    name_input: gtk::Entry,
    local_part_input: gtk::Entry,
    domain_picker: gtk::DropDown,
    description_input: gtk::Entry,
    list_help_input: gtk::Entry,
    list_archive_input: gtk::Entry,
    per_send_input: gtk::Entry,
    submit_button: gtk::Button,
    cancel_button: gtk::Button,
}

struct ListsCtx {
    machine: Arc<MailListsMachine>,
    rt: tokio::runtime::Handle,
    form_mode: Cell<FormMode>,
    editing_id: RefCell<Option<String>>,
    domains: RefCell<Vec<String>>,
    on_navigate_to_members: Rc<dyn Fn(String, String)>,
    w: ListsWidgets,
}

/// Build the "Lists" preferences page. `on_navigate_to_members(list_id_hex,
/// friendly_name)` is called when a row's Members button is clicked —
/// `settings_shell` composes it from `mail_list_members`'s `select` closure
/// plus the stack switch, mirroring the Personalization page's
/// `on_navigate_to_muted_words` callback pattern. Returns the page plus a
/// `refresh` entry point the settings shell wires to its on-visible hook
/// (mirrors `subscriptions`/`general`/`nests`/`account`) — the page is
/// observer-free, so the domain picker must re-read when it becomes visible.
pub fn build_mail_lists_page(
    on_navigate_to_members: Rc<dyn Fn(String, String)>,
) -> (adw::PreferencesPage, Rc<dyn Fn()>) {
    let page = adw::PreferencesPage::builder()
        .title(S::TITLE)
        .icon_name("mail-send-symbolic")
        .build();

    let top_group = adw::PreferencesGroup::builder()
        .title(S::TITLE)
        .description(S::DESCRIPTION)
        .build();
    top_group.set_header_suffix(Some(&super::marker("page-heading")));
    let error_label = gtk::Label::builder().visible(false).build();
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    let error_row = adw::ActionRow::builder().activatable(false).build();
    error_row.add_suffix(&error_label);
    top_group.add(&error_row);

    let add_button = gtk::Button::builder()
        .label(S::ADD_BUTTON)
        .valign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .build();
    set_test_id(&add_button, ids::MAIL_LISTS_ADD_BUTTON);
    let add_row = adw::ActionRow::builder().activatable(false).build();
    add_row.add_suffix(&add_button);
    top_group.add(&add_row);
    page.add(&top_group);

    let list_group = adw::PreferencesGroup::builder().title(S::TITLE).build();
    list_group.set_header_suffix(Some(&super::marker("mail-lists-list")));
    // Un-hydrated first paint must not claim "No lists yet" — the page does
    // not know that yet (`ui/README.md` rule 5). `render()` below overwrites
    // the title once a real snapshot lands.
    let list_placeholder = adw::ActionRow::builder().title(S::LOADING).build();
    list_group.add(&list_placeholder);
    page.add(&list_group);

    // --- Add/edit inline sheet (hidden) ---
    let sheet_group = adw::PreferencesGroup::builder()
        .title(S::FORM_TITLE)
        .build();
    sheet_group.set_visible(false);
    let sheet_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    sheet_box.set_margin_top(8);
    sheet_box.set_margin_bottom(8);

    let name_input = gtk::Entry::builder()
        .placeholder_text(S::NAME_PLACEHOLDER)
        .build();
    set_test_id(&name_input, ids::MAIL_LISTS_ADD_SHEET_NAME_INPUT);
    sheet_box.append(&name_input);

    let local_part_input = gtk::Entry::builder()
        .placeholder_text(S::LOCAL_PART_PLACEHOLDER)
        .build();
    set_test_id(
        &local_part_input,
        ids::MAIL_LISTS_ADD_SHEET_LOCAL_PART_INPUT,
    );
    sheet_box.append(&local_part_input);

    // mail-lists-add-sheet-domain-picker — DropDown of the user's owned domains.
    let domain_picker = gtk::DropDown::from_strings(&[]);
    set_test_id(&domain_picker, ids::MAIL_LISTS_ADD_SHEET_DOMAIN_PICKER);
    sheet_box.append(&domain_picker);

    let description_input = gtk::Entry::builder()
        .placeholder_text(S::DESCRIPTION_PLACEHOLDER)
        .build();
    set_test_id(
        &description_input,
        ids::MAIL_LISTS_ADD_SHEET_DESCRIPTION_INPUT,
    );
    sheet_box.append(&description_input);

    let list_help_input = gtk::Entry::builder()
        .placeholder_text(S::LIST_HELP_PLACEHOLDER)
        .build();
    set_test_id(
        &list_help_input,
        ids::MAIL_LISTS_ADD_SHEET_LIST_HELP_URL_INPUT,
    );
    sheet_box.append(&list_help_input);

    let list_archive_input = gtk::Entry::builder()
        .placeholder_text(S::LIST_ARCHIVE_PLACEHOLDER)
        .build();
    set_test_id(
        &list_archive_input,
        ids::MAIL_LISTS_ADD_SHEET_LIST_ARCHIVE_URL_INPUT,
    );
    sheet_box.append(&list_archive_input);

    let per_send_input = gtk::Entry::builder()
        .placeholder_text(S::PER_SEND_PLACEHOLDER)
        .build();
    set_test_id(
        &per_send_input,
        ids::MAIL_LISTS_ADD_SHEET_PER_SEND_CAP_INPUT,
    );
    sheet_box.append(&per_send_input);

    let submit_button = gtk::Button::builder()
        .label(S::SUBMIT)
        .css_classes(["suggested-action"])
        .build();
    set_test_id(&submit_button, ids::MAIL_LISTS_ADD_SHEET_SUBMIT_BUTTON);
    // `submit_sheet` dispatches `Create` in Add mode and `Update` in Edit mode
    // — mirrors `mail_aliases.rs`'s `MailAliasesSubmit` gap, closed the same
    // way: seed `Create` here (matching `FormMode::Add`, the seeded state),
    // then re-declare in `open_add_sheet`/`open_edit_sheet`, the only two
    // places that ever change the mode on this persistent widget (tui closed
    // its own `MailListsSubmit` twin).
    crate::offline_gate::declare_wire_kind(&submit_button, "fauna.bridges.create_account_list");
    sheet_box.append(&submit_button);
    let cancel_button = gtk::Button::builder().label(S::CANCEL).build();
    set_test_id(&cancel_button, ids::MAIL_LISTS_ADD_SHEET_CANCEL_BUTTON);
    sheet_box.append(&cancel_button);

    sheet_group.add(&sheet_box);
    page.add(&sheet_group);

    let widgets = ListsWidgets {
        error_label,
        add_button,
        list_group,
        list_placeholder,
        rows: Rc::new(RefCell::new(Vec::new())),
        sheet_group,
        name_input,
        local_part_input,
        domain_picker,
        description_input,
        list_help_input,
        list_archive_input,
        per_send_input,
        submit_button,
        cancel_button,
    };
    let refresh = wire_machine(widgets, on_navigate_to_members);

    (page, refresh)
}

fn wire_machine(
    widgets: ListsWidgets,
    on_navigate_to_members: Rc<dyn Fn(String, String)>,
) -> Rc<dyn Fn()> {
    let client = match crate::settings::get_client() {
        Some(c) => c,
        // No client (unit test / pre-auth): the page stays at its placeholder,
        // so becoming visible later has nothing to refresh.
        None => return Rc::new(|| {}),
    };
    let machine = Arc::new(crate::mail_glue::build_mail_lists_machine(&client));

    let ctx = Rc::new(ListsCtx {
        machine,
        rt: client.runtime_handle(),
        form_mode: Cell::new(FormMode::Add),
        editing_id: RefCell::new(None),
        domains: RefCell::new(Vec::new()),
        on_navigate_to_members,
        w: widgets,
    });

    hydrate_and_render(&ctx);

    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .add_button
            .clone()
            .connect_clicked(move |_| open_add_sheet(&ctx));
    }
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .submit_button
            .clone()
            .connect_clicked(move |_| submit_sheet(&ctx));
    }
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.cancel_button.clone().connect_clicked(move |_| {
            ctx.w.sheet_group.set_visible(false);
            hydrate_and_render(&ctx);
        });
    }

    // The settings shell's on-visible hook (mirrors subscriptions/general/
    // nests/account, `views/settings_shell.rs`): the page is
    // observer-free, so its `mail-lists-add-sheet-domain-picker` options —
    // derived from the caller's OWN alias/list rows — go stale the moment an
    // alias is added elsewhere (or on a fresh actor with none yet at the ONE
    // hydrate this ctx got at settings-shell build time) unless re-read here.
    Rc::new(move || hydrate_and_render(&ctx))
}

fn open_add_sheet(ctx: &Rc<ListsCtx>) {
    ctx.form_mode.set(FormMode::Add);
    *ctx.editing_id.borrow_mut() = None;
    crate::offline_gate::declare_wire_kind(
        &ctx.w.submit_button,
        "fauna.bridges.create_account_list",
    );
    let w = &ctx.w;
    w.name_input.set_text("");
    w.local_part_input.set_text("");
    w.local_part_input.set_sensitive(true);
    w.domain_picker.set_sensitive(true);
    w.description_input.set_text("");
    w.list_help_input.set_text("");
    w.list_archive_input.set_text("");
    w.per_send_input.set_text("");
    w.error_label.set_visible(false);
    w.sheet_group.set_visible(true);
}

fn open_edit_sheet(ctx: &Rc<ListsCtx>, view: &ListView) {
    ctx.form_mode.set(FormMode::Edit);
    *ctx.editing_id.borrow_mut() = Some(view.list_id_hex.clone());
    crate::offline_gate::declare_wire_kind(
        &ctx.w.submit_button,
        "fauna.bridges.update_account_list",
    );
    let w = &ctx.w;
    w.name_input.set_text(&view.friendly_name);
    // Address (local-part + domain) is immutable on edit — disable those fields.
    w.local_part_input.set_text(&view.local_part);
    w.local_part_input.set_sensitive(false);
    w.domain_picker.set_sensitive(false);
    w.description_input.set_text(&view.description);
    w.list_help_input.set_text(&view.list_help_url);
    w.list_archive_input.set_text(&view.list_archive_url);
    w.per_send_input.set_text(
        &view
            .recipients_per_send
            .map(|v| v.to_string())
            .unwrap_or_default(),
    );
    w.error_label.set_visible(false);
    w.sheet_group.set_visible(true);
}

fn submit_sheet(ctx: &Rc<ListsCtx>) {
    let w = &ctx.w;
    let domains = ctx.domains.borrow();
    let local_domain = domains
        .get(w.domain_picker.selected() as usize)
        .cloned()
        .unwrap_or_default();
    let draft = ListDraft {
        friendly_name: w.name_input.text().trim().to_string(),
        local_part: w.local_part_input.text().trim().to_string(),
        local_domain,
        description: w.description_input.text().trim().to_string(),
        list_help_url: w.list_help_input.text().trim().to_string(),
        list_archive_url: w.list_archive_input.text().trim().to_string(),
        recipients_per_send: fauna_core::format::parse_count(&w.per_send_input.text()),
    };
    let action = match ctx.form_mode.get() {
        FormMode::Add => MailListsAction::Create { draft },
        FormMode::Edit => {
            let Some(list_id_hex) = ctx.editing_id.borrow().clone() else {
                return;
            };
            MailListsAction::Update { list_id_hex, draft }
        }
    };

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
            if snap.error.is_none() {
                ctx_render.w.sheet_group.set_visible(false);
            }
        },
    );
}

fn hydrate_and_render(ctx: &Rc<ListsCtx>) {
    let machine = Arc::clone(&ctx.machine);
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

fn dispatch_action(ctx: &Rc<ListsCtx>, action: MailListsAction) {
    let machine = Arc::clone(&ctx.machine);
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

fn render(ctx: &Rc<ListsCtx>, snap: &MailListsSnapshot) {
    let w = &ctx.w;

    super::render_error_label(&w.error_label, snap.error.as_deref());

    // Domain picker options + the add-control gating (need an owned domain).
    *ctx.domains.borrow_mut() = snap.local_domains.clone();
    let domain_refs: Vec<&str> = snap.local_domains.iter().map(|s| s.as_str()).collect();
    w.domain_picker
        .set_model(Some(&gtk::StringList::new(&domain_refs)));
    let has_domain = !snap.local_domains.is_empty();
    w.add_button.set_sensitive(has_domain);
    if !has_domain && snap.error.is_none() {
        super::render_error_label(&w.error_label, Some(S::NO_DOMAIN));
    }

    {
        let mut rows = w.rows.borrow_mut();
        for row in rows.drain(..) {
            w.list_group.remove(&row);
        }
        for view in &snap.lists {
            let row = build_list_row(ctx, view);
            w.list_group.add(&row);
            rows.push(row);
        }
    }
    w.list_placeholder.set_title(S::EMPTY);
    w.list_placeholder.set_visible(snap.lists.is_empty());
}

fn build_list_row(ctx: &Rc<ListsCtx>, view: &ListView) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(&view.friendly_name)
        .subtitle(&view.address)
        .build();
    row.add_prefix(&super::marker("mail-lists-list-item"));

    // ui.yaml: "List friendly name + send-from address (e.g. \"Bob's Weekly —
    // bob-weekly@<domain>\")" — matches tui's `list_row_elements` format.
    row.add_suffix(&super::value_marker(
        "mail-lists-list-item-name",
        &format!("{} — {}", view.friendly_name, view.address),
    ));
    row.add_suffix(&super::value_marker(
        "mail-lists-list-item-member-count",
        &view.member_count.to_string(),
    ));
    row.add_suffix(&super::value_marker(
        "mail-lists-list-item-last-send",
        &view
            .last_send_at_ms
            .map(crate::i18n::local_date)
            .unwrap_or_default(),
    ));
    row.add_suffix(&super::value_marker(
        "mail-lists-list-item-quota",
        &format!("{}/{}", view.sends_today, view.recipients_today),
    ));

    let edit_button = gtk::Button::builder()
        .label(S::EDIT)
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    set_test_id(&edit_button, ids::MAIL_LISTS_LIST_ITEM_EDIT_BUTTON);
    {
        let ctx = Rc::clone(ctx);
        let view = view.clone();
        edit_button.connect_clicked(move |_| open_edit_sheet(&ctx, &view));
    }
    row.add_suffix(&edit_button);

    // mail-lists-list-item-members-button — scopes the mail-list-members page
    // to this row's list, then routes to it (tui's prior art:
    // `Action::MailListsOpenMembers`, `apps/fauna-tui/src/settings/mod.rs`).
    let members_button = gtk::Button::builder()
        .label(S::MEMBERS)
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    set_test_id(&members_button, ids::MAIL_LISTS_LIST_ITEM_MEMBERS_BUTTON);
    {
        let on_navigate_to_members = Rc::clone(&ctx.on_navigate_to_members);
        let list_id_hex = view.list_id_hex.clone();
        let friendly_name = view.friendly_name.clone();
        members_button.connect_clicked(move |_| {
            on_navigate_to_members(list_id_hex.clone(), friendly_name.clone());
        });
    }
    row.add_suffix(&members_button);

    let delete_button = gtk::Button::builder()
        .label(S::DELETE)
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    set_test_id(&delete_button, ids::MAIL_LISTS_LIST_ITEM_DELETE_BUTTON);
    crate::offline_gate::declare_wire_kind(&delete_button, "fauna.bridges.delete_account_list");
    {
        let ctx = Rc::clone(ctx);
        let list_id_hex = view.list_id_hex.clone();
        super::wire_two_click(
            &delete_button,
            S::DELETE,
            S::DELETE_CONFIRM,
            false,
            |btn| btn.add_css_class("destructive-action"),
            |btn| btn.remove_css_class("destructive-action"),
            move || {
                dispatch_action(
                    &ctx,
                    MailListsAction::Delete {
                        list_id_hex: list_id_hex.clone(),
                    },
                )
            },
        );
    }
    row.add_suffix(&delete_button);

    row
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::testid::widget_names;

    #[test]
    fn lists_page_exposes_static_ui_yaml_ids() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let (page, _refresh) = build_mail_lists_page(Rc::new(|_, _| {}));
            let names = widget_names(&page);
            for id in [
                "page-heading",
                "error-message",
                "mail-lists-add-button",
                "mail-lists-add-sheet-name-input",
                "mail-lists-add-sheet-local-part-input",
                "mail-lists-add-sheet-domain-picker",
                "mail-lists-add-sheet-description-input",
                "mail-lists-add-sheet-list-help-url-input",
                "mail-lists-add-sheet-list-archive-url-input",
                "mail-lists-add-sheet-per-send-cap-input",
                "mail-lists-add-sheet-submit-button",
                "mail-lists-add-sheet-cancel-button",
                "mail-lists-list",
            ] {
                assert!(
                    names.iter().any(|n| n == id),
                    "missing widget id {id:?}; have {names:?}"
                );
            }
        });
    }
}
