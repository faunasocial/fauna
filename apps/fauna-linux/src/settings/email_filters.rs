//! The "Email filters" group on the Privacy settings page — sieve-style mail
//! filter CRUD (list, create, edit, delete) over `fauna.email.filters.*`
//! (`docs/goal/behavior/smtp-server.md` § Email filter rules).
//!
//! Migrated off the legacy `client.rs`/`DataMessage` plumbing
//! (`FaunaClient::fetch_email_filters` dispatched `DataMessage::EmailFiltersLoaded`,
//! which `app.rs` only logged — the rendered rows never carried a real filter
//! id, so `filter-delete` removed the row locally and never called the
//! server at all) onto `muted_words.rs`'s self-contained `Widgets`/`Ctx` +
//! `spawn_with_snapshot` shape (priority #4 — richest existing pattern). The
//! create/edit dropdown↔wire mapping is the shared
//! `fauna_protocol::email::{encode_filter_rule, encode_filter_action,
//! describe_filter_rule, describe_filter_action, filter_is_editable_for}` every
//! app's dialog shares — the Rust-native linux app calls it directly. The form
//! holds the whole [`FilterActionInputs`] (the Forward destination and copy
//! mode, plus the Reject reason an edit writes back unchanged), so it offers
//! and opens every `SUPPORTED_ACTION_KINDS` entry.
//!
//! **The post-succession review mark, and its Keep half** (`succession-aftermath.md` § Adjudicating what the aftermath carries
//! across, the fourth plane) — ported near-verbatim from tui's
//! `settings/privacy.rs` + `settings/mod.rs`'s `Op::DeleteFilter`/
//! `Op::KeepFilter`. `fauna_client_config::{load_filter_marks,
//! decide_filter_mark}` are called in-process over
//! `crate::account_runtime::ledger_store` (linux is Rust-native, same as tui
//! — no FFI/wasm boundary). There is deliberately no
//! `filter-review-remove-button`: `filter-delete` is the Remove half
//! (no-second-removal-mechanism rule), so a delete of a MARKED row also
//! records the verdict, ordered **after** the deletion — a failure there
//! leaves a re-asked question, never a silenced armed rule.

use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;

use fauna_client_email::EmailClient;
use fauna_core::data::UnattestedVerdict;
use fauna_protocol::email::{
    EmailFilter, EmailFilterAction, EmailFilterRule, FilterActionInputs,
    SUPPORTED_ACTION_KINDS as ACTION_KINDS, SUPPORTED_RULE_KINDS as RULE_KINDS,
    describe_filter_action, describe_filter_rule, encode_filter_action, encode_filter_rule,
    filter_is_editable_for,
};

use crate::async_helper::spawn_with_snapshot;
use crate::client::FaunaClient;
use crate::i18n::strings::common;
use crate::i18n::strings::settings::{self, errors as settings_errors, privacy_page};
use crate::testid::set_test_id;

type Nest = Arc<fauna_client::NestClient>;
type LoadResult = Result<Vec<EmailFilter>, String>;

/// The shared create/edit form. `create_btn`/`save_btn` are mutually
/// exclusive (visibility toggled by `Ctx::editing_id`) — distinct ui.yaml IDs
/// (`create-filter` / `save-filter`) so create vs. edit stay semantically
/// separate on every app (2026-07-16 user-approved shape).
struct FormWidgets {
    container: gtk::Box,
    name_entry: gtk::Entry,
    rule_type: gtk::DropDown,
    value_entry: gtk::Entry,
    action_dd: gtk::DropDown,
    /// `filter-forward-address` — shown only while the action is Forward.
    forward_address: gtk::Entry,
    /// `filter-keep-local-copy` — checked by default (copy); unchecked is a
    /// redirect. Shown only while the action is Forward.
    keep_local_copy: gtk::CheckButton,
    create_btn: gtk::Button,
    save_btn: gtk::Button,
}

struct Widgets {
    error_label: gtk::Label,
    /// A plain `gtk::Box`, NOT a `gtk::ListBox` — `ListBox::append` wraps
    /// each child in an auto-generated `GtkListBoxRow`, so a later
    /// `list.remove(&row)` (removing the raw row, not the wrapper) fails as
    /// "tried to remove non-child" (a GTK warning, not a panic) and leaves
    /// the stale row behind — exactly the bug that let an edited filter's
    /// old name linger alongside the new one. Mirrors `muted_words.rs`.
    list: gtk::Box,
    empty: gtk::Label,
    add_btn: gtk::Button,
    form: FormWidgets,
    rows: RefCell<Vec<gtk::Box>>,
    /// `Some(id)` while the form edits an existing filter; `None` in create mode.
    editing_id: RefCell<Option<i64>>,
    /// The Reject reason of the filter being edited. The form has no field
    /// for it, so it rides here and is written back unchanged on save (empty
    /// in create mode — the encoder's default reason).
    reject_reason: RefCell<String>,
}

struct Ctx {
    nest: Nest,
    rt: tokio::runtime::Handle,
    /// The cached open inherited-rule marks — the ids `build_filter_row`
    /// paints `filter-unattested-mark`/`filter-review-keep-button` for. A failed
    /// re-read leaves this alone rather than blanking it (mirrors tui's
    /// `read_filter_marks`'s `None` = "leave the cache alone" contract) — a
    /// transport blip must never silently hide a mark the owner has not answered.
    filter_marks: RefCell<Vec<i64>>,
    w: Widgets,
}

/// Build the "Email filters" preferences group (list, create, edit, delete),
/// plus its re-read — the Privacy page's nav edge calls it, so a rule or an
/// inherited-rule mark that landed after the build-once shell mounted (the
/// succession aftermath raises marks while the successor's shell is already
/// up) shows on the next visit instead of never.
pub fn build_email_filters_group(
    client: &Rc<FaunaClient>,
) -> (adw::PreferencesGroup, Rc<dyn Fn()>) {
    let (group, widgets) = build_group_widgets();
    let ctx = wire(client, widgets);
    let refresh_fn: Rc<dyn Fn()> = Rc::new(move || refresh(&ctx));
    (group, refresh_fn)
}

/// Build the static widget tree (every ui.yaml ID present) with no client
/// dependency, split out so a unit test can exercise ID-conformance without a
/// real `FaunaClient` (mirrors `muted_words.rs`'s `build_page_widgets` split).
fn build_group_widgets() -> (adw::PreferencesGroup, Widgets) {
    let group = adw::PreferencesGroup::builder()
        .title(privacy_page::EMAIL_FILTERS)
        .description(privacy_page::EMAIL_FILTERS_DESCRIPTION)
        .build();

    // error-message (Rule 2) — this group's page (Privacy) had none before
    // this migration; every mutation below can fail over the wire, so it
    // needs one.
    let error_label = gtk::Label::builder().visible(false).build();
    error_label.add_css_class("error");
    error_label.set_halign(gtk::Align::Start);
    error_label.set_wrap(true);
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    group.add(&error_label);

    let list = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .css_classes(["boxed-list"])
        .build();
    group.add(&list);

    let empty = gtk::Label::new(Some(&format!(
        "{}\n{}",
        privacy_page::NO_FILTERS_CONFIGURED,
        privacy_page::NO_FILTERS_SUBTITLE
    )));
    empty.add_css_class("dim-label");
    empty.set_halign(gtk::Align::Start);
    group.add(&empty);

    let add_row = adw::ActionRow::builder()
        .title(privacy_page::ADD_FILTER)
        .activatable(true)
        .build();
    let add_btn = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .valign(gtk::Align::Center)
        .build();
    set_test_id(&add_btn, ids::ADD_FILTER_BTN);
    add_row.add_suffix(&add_btn);
    group.add(&add_row);

    let form = build_form_widgets();
    group.add(&form.container);

    let widgets = Widgets {
        error_label,
        list,
        empty,
        add_btn,
        form,
        rows: RefCell::new(Vec::new()),
        editing_id: RefCell::new(None),
        reject_reason: RefCell::new(String::new()),
    };
    (group, widgets)
}

fn build_form_widgets() -> FormWidgets {
    let container = gtk::Box::new(gtk::Orientation::Vertical, 8);
    container.set_margin_top(8);
    container.set_margin_bottom(8);
    container.set_visible(false);

    let name_entry = gtk::Entry::builder()
        .placeholder_text(settings::FILTER_NAME)
        .build();
    set_test_id(&name_entry, ids::FILTER_NAME_INPUT);
    container.append(&name_entry);

    // Option values are the wire enum names (matching web's `<option value>`),
    // so the e2e `select(value)` and the constructed rule/action line up.
    let rule_fields = gtk::StringList::new(RULE_KINDS);
    let rule_type = gtk::DropDown::builder().model(&rule_fields).build();
    set_test_id(&rule_type, ids::FILTER_RULE_TYPE);
    container.append(&rule_type);

    let value_entry = gtk::Entry::builder()
        .placeholder_text(privacy_page::MATCH_VALUE)
        .build();
    set_test_id(&value_entry, ids::FILTER_RULE_VALUE);
    container.append(&value_entry);

    let action_values = gtk::StringList::new(ACTION_KINDS);
    let action_dd = gtk::DropDown::builder().model(&action_values).build();
    set_test_id(&action_dd, ids::FILTER_ACTION_SELECT);
    container.append(&action_dd);

    // The Forward action's own inputs (`mail-forwarding.md` § Per-rule
    // "forward to"): present only while Forward is the selected action.
    let forward_address = gtk::Entry::builder()
        .placeholder_text(privacy_page::FORWARD_ADDRESS)
        .visible(false)
        .build();
    set_test_id(&forward_address, ids::FILTER_FORWARD_ADDRESS);
    container.append(&forward_address);

    let keep_local_copy = gtk::CheckButton::builder()
        .label(privacy_page::KEEP_LOCAL_COPY)
        .active(true)
        .visible(false)
        .build();
    set_test_id(&keep_local_copy, ids::FILTER_KEEP_LOCAL_COPY);
    container.append(&keep_local_copy);

    {
        let forward_address = forward_address.clone();
        let keep_local_copy = keep_local_copy.clone();
        action_dd.connect_selected_notify(move |dd| {
            let forward = dropdown_value(dd) == "Forward";
            forward_address.set_visible(forward);
            keep_local_copy.set_visible(forward);
        });
    }

    let create_btn = gtk::Button::with_label(common::CREATE);
    create_btn.add_css_class("suggested-action");
    set_test_id(&create_btn, ids::CREATE_FILTER);
    crate::offline_gate::declare_wire_kind(&create_btn, "fauna.email.filters.create");
    container.append(&create_btn);

    let save_btn = gtk::Button::with_label(common::SAVE);
    save_btn.add_css_class("suggested-action");
    save_btn.set_visible(false);
    set_test_id(&save_btn, ids::SAVE_FILTER);
    crate::offline_gate::declare_wire_kind(&save_btn, "fauna.email.filters.update");
    container.append(&save_btn);

    FormWidgets {
        container,
        name_entry,
        rule_type,
        value_entry,
        action_dd,
        forward_address,
        keep_local_copy,
        create_btn,
        save_btn,
    }
}

fn wire(client: &Rc<FaunaClient>, widgets: Widgets) -> Rc<Ctx> {
    let ctx = Rc::new(Ctx {
        nest: client.nest_rpc().clone(),
        rt: client.runtime_handle(),
        filter_marks: RefCell::new(Vec::new()),
        w: widgets,
    });

    refresh(&ctx);

    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .add_btn
            .clone()
            .connect_clicked(move |_| open_form_for_create(&ctx));
    }
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .form
            .create_btn
            .clone()
            .connect_clicked(move |_| submit_create(&ctx));
    }
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .form
            .save_btn
            .clone()
            .connect_clicked(move |_| submit_save(&ctx));
    }
    ctx
}

/// Load the current filter list from the nest on mount, and the open
/// inherited-rule marks alongside it, best-effort. `load_filters`
/// is a single NestClient RPC — the transport already parks it while the
/// socket comes up (transport.md § Request lifecycle step 3).
fn refresh(ctx: &Rc<Ctx>) {
    let nest = ctx.nest.clone();
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            let filters = load_filters(nest.clone()).await;
            let marks = read_filter_marks().await;
            (filters, marks)
        },
        move |(filters, marks)| apply_load(&ctx_render, filters, marks),
    );
}

async fn load_filters(nest: Nest) -> LoadResult {
    EmailClient::new(nest)
        .filters_list()
        .await
        .map_err(|e| e.to_string())
}

/// Read the open inherited-rule marks back — `None` on a read failure, which
/// the caller treats as "leave the cache alone" (see [`Ctx::filter_marks`]'s
/// own doc for why collapsing that into an empty list is the wrong direction).
async fn read_filter_marks() -> Option<Vec<i64>> {
    let store = crate::account_runtime::ledger_store().ok()?;
    match fauna_client_config::load_filter_marks(&store).await {
        Ok(ids) => Some(ids),
        Err(e) => {
            tracing::warn!(error = %e, "re-reading the inherited-filter marks failed");
            None
        }
    }
}

/// Cache a fresh marks read, and hand its count to the Recovery kit section's
/// inherited-filters line — one read feeding both, so a Keep or a delete here
/// takes the line down with the mark instead of leaving it at the aftermath's
/// raise-time count until the next launch.
fn store_marks(ctx: &Rc<Ctx>, marks: Vec<i64>) {
    crate::settings::apply_aftermath_progress(
        crate::settings::recovery_kit::AftermathUpdate::InheritedFilters(marks.len()),
    );
    *ctx.filter_marks.borrow_mut() = marks;
}

fn apply_load(ctx: &Rc<Ctx>, result: LoadResult, marks: Option<Vec<i64>>) {
    if let Some(marks) = marks {
        store_marks(ctx, marks);
    }
    match result {
        Ok(filters) => {
            clear_error(ctx);
            render_rows(ctx, &filters);
        }
        Err(msg) => show_error(ctx, &msg),
    }
}

/// Rebuild the `filter-item` row list from the persisted filter list.
fn render_rows(ctx: &Rc<Ctx>, filters: &[EmailFilter]) {
    let mut rows = ctx.w.rows.borrow_mut();
    for row in rows.drain(..) {
        ctx.w.list.remove(&row);
    }
    for filter in filters {
        let row = build_filter_row(ctx, filter);
        ctx.w.list.append(&row);
        rows.push(row);
    }
    ctx.w.empty.set_visible(filters.is_empty());
}

/// Build one `filter-item` row: name/action markers + edit (gated
/// `filter_is_editable`) + delete.
///
/// A plain `gtk::Box` (not `adw::ActionRow`), mirroring `muted_words.rs`'s
/// `build_word_row` exactly — `filter-item` is stamped on the row itself so
/// it's a real ancestor of `filter-edit`/`filter-delete` for scoped e2e
/// queries (`scope="filter-item[i]"`). An `adw::ActionRow` with `filter-item`
/// only on a 1px marker *sibling* Label — the shape this replaces — has no
/// descendants at all, so a scoped lookup into it always comes up empty; that
/// gap was latent because every prior read here was global (`filter_count`/
/// `filter_names`/`delete_filter` never scoped), only surfacing once
/// filter-edit's gated-visibility check needed a real per-row scope.
fn build_filter_row(ctx: &Rc<Ctx>, filter: &EmailFilter) -> gtk::Box {
    let action_text = crate::i18n::email_filter_action_label(&filter.action);
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&row, ids::FILTER_ITEM);

    let name_label = gtk::Label::new(Some(&filter.name));
    name_label.set_hexpand(true);
    name_label.set_halign(gtk::Align::Start);
    set_test_id(&name_label, ids::FILTER_NAME);
    row.append(&name_label);

    let action_label = gtk::Label::new(Some(&action_text));
    set_test_id(&action_label, ids::FILTER_ACTION);
    row.append(&action_label);

    // Gated: a filter only a raw API call could have produced (multi-rule, or
    // a richer rule/action no dialog collects) never opens a form that would
    // silently narrow it on save.
    if filter_is_editable_for(&filter.rules, &filter.action, ACTION_KINDS) {
        let edit_btn = gtk::Button::builder()
            .icon_name("document-edit-symbolic")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        set_test_id(&edit_btn, ids::FILTER_EDIT);
        crate::offline_gate::declare_wire_kind(&edit_btn, "fauna.email.filters.get");
        {
            let ctx = Rc::clone(ctx);
            let id = filter.id;
            edit_btn.connect_clicked(move |_| open_form_for_edit(&ctx, id));
        }
        row.append(&edit_btn);
    }

    // ── The post-succession review mark, and its Keep half ──
    //
    // Renders ONLY on a rule the aftermath carried across and the owner has
    // not adjudicated. There is deliberately no `filter-review-remove-button`
    // — `filter-delete`, one line down, is the Remove half
    // (no-second-removal-mechanism rule); recording Removed without deleting
    // would leave an armed rule under a list that now reads clean.
    if ctx.filter_marks.borrow().contains(&filter.id) {
        let mark_label = gtk::Label::new(Some(privacy_page::FILTER_INHERITED));
        mark_label.add_css_class("dim-label");
        set_test_id(&mark_label, ids::FILTER_UNATTESTED_MARK);
        row.append(&mark_label);

        let keep_btn = gtk::Button::with_label(privacy_page::FILTER_KEEP);
        keep_btn.set_valign(gtk::Align::Center);
        set_test_id(&keep_btn, ids::FILTER_REVIEW_KEEP_BUTTON);
        crate::offline_gate::declare_wire_kind(&keep_btn, "fauna.account.state.put");
        {
            let ctx = Rc::clone(ctx);
            let id = filter.id;
            keep_btn.connect_clicked(move |_| dispatch_keep_filter_mark(&ctx, id));
        }
        row.append(&keep_btn);
    }

    let delete_btn = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    set_test_id(&delete_btn, ids::FILTER_DELETE);
    crate::offline_gate::declare_wire_kind(&delete_btn, "fauna.email.filters.delete");
    {
        let ctx = Rc::clone(ctx);
        let id = filter.id;
        delete_btn.connect_clicked(move |_| submit_delete(&ctx, id));
    }
    row.append(&delete_btn);

    row
}

fn open_form_for_create(ctx: &Rc<Ctx>) {
    *ctx.w.editing_id.borrow_mut() = None;
    clear_error(ctx);
    ctx.w.form.name_entry.set_text("");
    ctx.w.form.value_entry.set_text("");
    ctx.w.form.rule_type.set_selected(0);
    ctx.w.form.action_dd.set_selected(0);
    apply_inputs(ctx, &FilterActionInputs::default());
    ctx.w.form.create_btn.set_visible(true);
    ctx.w.form.save_btn.set_visible(false);
    ctx.w.form.container.set_visible(true);
}

/// Open the form pre-populated for an existing filter — a fresh
/// `filters_get` (not the cached list row), so the edit reflects the current
/// server state.
fn open_form_for_edit(ctx: &Rc<Ctx>, id: i64) {
    let nest = ctx.nest.clone();
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            EmailClient::new(nest)
                .filters_get(id)
                .await
                .map_err(|e| e.to_string())
        },
        move |result| apply_get(&ctx_render, result),
    );
}

fn apply_get(ctx: &Rc<Ctx>, result: Result<EmailFilter, String>) {
    let filter = match result {
        Ok(f) => f,
        Err(msg) => {
            show_error(ctx, &msg);
            return;
        }
    };
    let described = filter
        .rules
        .first()
        .and_then(describe_filter_rule)
        .zip(describe_filter_action(&filter.action));
    let Some(((rule_kind, value), inputs)) = described else {
        // The row's own filter_is_editable gate should have prevented this —
        // only reachable if the filter changed server-side between list-load
        // and the edit click.
        show_error(ctx, settings_errors::UPDATE_FILTER);
        return;
    };

    clear_error(ctx);
    *ctx.w.editing_id.borrow_mut() = Some(filter.id);
    ctx.w.form.name_entry.set_text(&filter.name);
    ctx.w.form.value_entry.set_text(&value);
    select_dropdown(&ctx.w.form.rule_type, RULE_KINDS, rule_kind);
    select_dropdown(&ctx.w.form.action_dd, ACTION_KINDS, &inputs.kind);
    apply_inputs(ctx, &inputs);
    ctx.w.form.create_btn.set_visible(false);
    ctx.w.form.save_btn.set_visible(true);
    ctx.w.form.container.set_visible(true);
}

/// Load every non-dropdown action input from `inputs` into the form (the
/// dropdown itself is the caller's, so it can select the kind first).
fn apply_inputs(ctx: &Rc<Ctx>, inputs: &FilterActionInputs) {
    ctx.w.form.forward_address.set_text(&inputs.forward_address);
    ctx.w
        .form
        .keep_local_copy
        .set_active(inputs.keep_local_copy);
    *ctx.w.reject_reason.borrow_mut() = inputs.reject_reason.clone();
}

fn select_dropdown(dd: &gtk::DropDown, values: &[&str], selected: &str) {
    if let Some(pos) = values.iter().position(|v| *v == selected) {
        dd.set_selected(pos as u32);
    }
}

/// Read the dropdown's currently-selected option's wire value (matches web's
/// `<option value>`).
fn dropdown_value(dd: &gtk::DropDown) -> String {
    dd.selected_item()
        .and_then(|o| o.downcast::<gtk::StringObject>().ok())
        .map(|s| s.string().to_string())
        .unwrap_or_default()
}

/// Read + encode the form fields, surfacing an unknown-kind error instead of
/// the pre-lift clients' silent coercion.
fn read_form(ctx: &Rc<Ctx>) -> Option<(String, EmailFilterRule, EmailFilterAction)> {
    let name = ctx.w.form.name_entry.text().to_string();
    let value = ctx.w.form.value_entry.text().to_string();
    let rule_kind = dropdown_value(&ctx.w.form.rule_type);
    let inputs = FilterActionInputs {
        kind: dropdown_value(&ctx.w.form.action_dd),
        reject_reason: ctx.w.reject_reason.borrow().clone(),
        forward_address: ctx.w.form.forward_address.text().to_string(),
        keep_local_copy: ctx.w.form.keep_local_copy.is_active(),
    };

    let rule = match encode_filter_rule(&rule_kind, &value) {
        Ok(r) => r,
        Err(e) => {
            show_error(ctx, &e.to_string());
            return None;
        }
    };
    let action = match encode_filter_action(&inputs) {
        Ok(a) => a,
        Err(e) => {
            show_error(ctx, &e.to_string());
            return None;
        }
    };
    Some((name, rule, action))
}

fn submit_create(ctx: &Rc<Ctx>) {
    if let Some((name, rule, action)) = read_form(ctx) {
        dispatch(ctx, Mutation::Create { name, rule, action });
    }
}

fn submit_save(ctx: &Rc<Ctx>) {
    let Some(id) = *ctx.w.editing_id.borrow() else {
        return;
    };
    if let Some((name, rule, action)) = read_form(ctx) {
        dispatch(
            ctx,
            Mutation::Update {
                id,
                name,
                rule,
                action,
            },
        );
    }
}

fn submit_delete(ctx: &Rc<Ctx>, id: i64) {
    // ⚠ Ordered AFTER the delete inside `run_mutation`, deliberately — a
    // failure to record the verdict must leave a re-asked question, never a
    // silenced armed rule (this module's own doc comment, and tui's
    // `Op::DeleteFilter` the same way).
    let was_marked = ctx.filter_marks.borrow().contains(&id);
    dispatch(ctx, Mutation::Delete { id, was_marked });
}

enum Mutation {
    Create {
        name: String,
        rule: EmailFilterRule,
        action: EmailFilterAction,
    },
    Update {
        id: i64,
        name: String,
        rule: EmailFilterRule,
        action: EmailFilterAction,
    },
    Delete {
        id: i64,
        was_marked: bool,
    },
}

fn dispatch(ctx: &Rc<Ctx>, mutation: Mutation) {
    let nest = ctx.nest.clone();
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move { run_mutation(nest, mutation).await },
        move |result| apply_mutation(&ctx_render, result),
    );
}

/// Run the mutation, then re-list — the response the row list renders from,
/// so a create/edit/delete always reflects the server's real ids (no more
/// locally-guessed rows with no id, the bug this migration fixes). The second
/// element is `Some` only when a delete recorded a mark verdict — `None`
/// leaves [`Ctx::filter_marks`]'s cache alone, same contract as `refresh`'s.
async fn run_mutation(nest: Nest, mutation: Mutation) -> (LoadResult, Option<Vec<i64>>) {
    let email = EmailClient::new(nest.clone());
    let mut new_marks = None;
    let outcome: Result<(), String> = async {
        match mutation {
            Mutation::Create { name, rule, action } => {
                email
                    .filters_create(name, vec![rule], "all", action, 0)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            Mutation::Update {
                id,
                name,
                rule,
                action,
            } => {
                email
                    .filters_update(id, name, vec![rule], "all", action, 0)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            Mutation::Delete { id, was_marked } => {
                email.filters_delete(id).await.map_err(|e| e.to_string())?;
                if was_marked {
                    new_marks = record_filter_removal(id).await;
                }
            }
        }
        Ok(())
    }
    .await;
    if let Err(e) = outcome {
        return (Err(e), new_marks);
    }
    (
        email.filters_list().await.map_err(|e| e.to_string()),
        new_marks,
    )
}

/// Record **Removed** on a just-deleted marked filter, then read the marks
/// back — the twin of tui's `record_filter_removal`. Best-effort: the delete
/// already succeeded (the outcome that matters), so a failure here is logged
/// and leaves the mark open, re-asking the owner about a rule that no longer
/// exists — untidy, and strictly the safe direction.
async fn record_filter_removal(id: i64) -> Option<Vec<i64>> {
    let store = crate::account_runtime::ledger_store().ok()?;
    if let Err(e) =
        fauna_client_config::decide_filter_mark(&store, id, UnattestedVerdict::Removed).await
    {
        tracing::warn!(error = %e, "recording the inherited-rule removal failed");
    }
    fauna_client_config::load_filter_marks(&store).await.ok()
}

/// Record the owner's **Keep** verdict, then re-fetch the filter list AND
/// the marks together (`refresh`'s own combined shape) — the answered row's
/// mark clears on the next render.
fn dispatch_keep_filter_mark(ctx: &Rc<Ctx>, id: i64) {
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move { keep_filter_mark(id).await },
        move |result| match result {
            Ok(()) => {
                clear_error(&ctx_render);
                refresh(&ctx_render);
            }
            Err(msg) => show_error(&ctx_render, &msg),
        },
    );
}

async fn keep_filter_mark(id: i64) -> Result<(), String> {
    let store = crate::account_runtime::ledger_store()?;
    // Surfaced rather than swallowed (convention 11): a Keep that silently
    // did not land leaves the owner believing they answered a question that
    // will be asked again.
    fauna_client_config::decide_filter_mark(&store, id, UnattestedVerdict::Kept)
        .await
        .map_err(|e| format!("recording that you recognise this rule failed: {e}"))?;
    Ok(())
}

fn apply_mutation(ctx: &Rc<Ctx>, result: (LoadResult, Option<Vec<i64>>)) {
    let (result, marks) = result;
    if let Some(marks) = marks {
        store_marks(ctx, marks);
    }
    match result {
        Ok(filters) => {
            clear_error(ctx);
            close_form(ctx);
            render_rows(ctx, &filters);
        }
        Err(msg) => show_error(ctx, &msg),
    }
}

fn close_form(ctx: &Rc<Ctx>) {
    *ctx.w.editing_id.borrow_mut() = None;
    ctx.w.form.name_entry.set_text("");
    ctx.w.form.value_entry.set_text("");
    apply_inputs(ctx, &FilterActionInputs::default());
    ctx.w.form.container.set_visible(false);
}

fn show_error(ctx: &Rc<Ctx>, msg: &str) {
    super::render_error_label(&ctx.w.error_label, Some(msg));
}

fn clear_error(ctx: &Rc<Ctx>) {
    super::render_error_label(&ctx.w.error_label, None);
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::testid::widget_names;

    /// The group exposes every static ui.yaml ID with no registered client
    /// (`build_email_filters_group` requires a real `FaunaClient` for `wire`,
    /// so this test exercises the client-free `build_group_widgets` split).
    /// Per-row IDs (`filter-item`/`filter-name`/`filter-action`/
    /// `filter-delete`/`filter-edit`) only exist once data loads and aren't
    /// covered here — same boundary `muted_words.rs`'s equivalent test draws.
    #[test]
    fn email_filters_group_exposes_static_ui_yaml_ids() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();

            let (group, _widgets) = build_group_widgets();
            let names = widget_names(&group);
            for id in [
                "error-message",
                "add-filter-btn",
                "filter-name-input",
                "filter-rule-type",
                "filter-rule-value",
                "filter-action-select",
                "filter-forward-address",
                "filter-keep-local-copy",
                "create-filter",
                "save-filter",
            ] {
                assert!(
                    names.iter().any(|n| n == id),
                    "missing widget id {id:?}; have {names:?}"
                );
            }
        });
    }
}
