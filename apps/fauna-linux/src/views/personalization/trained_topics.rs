//! The **Trained topics** facet of the Personalization home
//! (`docs/goal/behavior/topic-factors.md` § Authoring surface & picker): list
//! the user's trained topic factors (name + example count), create, rename,
//! delete.
//!
//! **This file is a GTK shell, not a lifecycle.** The registry↔model-plane
//! sequencing — the advisory example-count read, the create cap, and the
//! delete's registry-removal-then-`model.delete` pairing — lives once in
//! `fauna_client_personalization::topics::TrainedTopics`, which every app
//! binds to (priority #2: linux natively, web through wasm, the natives through
//! UniFFI). What is left here is genuinely platform: build the service,
//! localize its typed errors, render each round-trip's rows.
//!
//! **One `name-input`, two flows (the ratified ui.yaml shape):**
//! `personalization-trained-factor-name-input` +
//! `personalization-trained-factor-create-button` create a factor inline
//! (the muted-words add-row pattern); a row's
//! `personalization-trained-factor-rename-button` retargets that same input
//! at the row (prefilled, button label flips to "Save name") and the next
//! commit renames instead of creating. Any successful commit resets the input
//! to create mode.
//!
//! Deleting pairs the registry removal with
//! `fauna.personalization.model.delete` on the derived key — compositions
//! still referencing the key stay valid (the zero-term seam makes an orphan
//! key inert, § Delete semantics).

use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;

// Aliased: this file's own facet struct is also called `TrainedTopics` (the GTK
// widget), while the shared one is the lifecycle service behind it.
use fauna_client_personalization::{
    TrainedTopicRow, TrainedTopics as TopicsService, TrainedTopicsError,
};
use fauna_sync_engine::preference_surfaces as plane;

use super::publish_sheet::{self, PublishSheet, PublishSheetWidgets};
use crate::async_helper::{hydrate_with_retry, spawn_with_snapshot};
use crate::client::FaunaClient;
use crate::i18n::strings::personalization as S;
use crate::testid::set_test_id;

type Nest = Arc<fauna_client::NestClient>;

// The row the facet renders is the shared crate's `TrainedTopicRow` (registry
// meta + the nest-side advisory example count) — no local mirror of it.

/// One async round-trip's result: the freshly-persisted registry (with
/// counts), or an error string for the page `error-message`.
type FacetResult = Result<Vec<TrainedTopicRow>, String>;

/// Static widget handles (built client-free for the ID-conformance test).
pub struct TrainedTopicsWidgets {
    /// The `adw::PreferencesGroup` the home page appends.
    pub group: adw::PreferencesGroup,
    input: gtk::Entry,
    create_button: gtk::Button,
    list: gtk::ListBox,
    empty: gtk::Label,
    /// The single-instance publish review-prune sheet, hidden until a row's
    /// publish button reveals it against that row's factor.
    publish: PublishSheetWidgets,
}

/// Everything the handlers + render need.
struct Ctx {
    nest: Nest,
    rt: tokio::runtime::Handle,
    w: TrainedTopicsWidgets,
    /// Page-level `error-message` label (shared with the labelers facet — one
    /// error element per page, e2e Rule 2).
    error_label: gtk::Label,
    /// `Some(factor id)` while the name-input is retargeted at a rename.
    renaming: RefCell<Option<Vec<u8>>>,
    /// The wired publish sheet a row's publish button opens. Set once, at
    /// [`wire`] — the sheet needs the same client this facet holds.
    publish: RefCell<Option<Rc<PublishSheet>>>,
}

/// Build the facet's static widget tree — every static ui.yaml ID present, no
/// client dependency (the `*_exposes_static_ui_yaml_ids` test builds this).
pub fn build_widgets() -> TrainedTopicsWidgets {
    let group = adw::PreferencesGroup::builder()
        .title(S::TRAINED_TOPICS_TITLE)
        .build();

    let input = gtk::Entry::builder()
        .placeholder_text(S::TRAINED_FACTOR_PLACEHOLDER)
        .hexpand(true)
        .build();
    set_test_id(&input, ids::PERSONALIZATION_TRAINED_FACTOR_NAME_INPUT);

    let create_button = gtk::Button::with_label(S::TRAINED_FACTOR_CREATE);
    create_button.add_css_class("suggested-action");
    create_button.set_valign(gtk::Align::Center);
    set_test_id(
        &create_button,
        ids::PERSONALIZATION_TRAINED_FACTOR_CREATE_BUTTON,
    );
    // Also the commit for a rename in progress (the row's "Rename" button
    // retargets this same input/button pair, `wire`'s `renaming` field) — both
    // flows write the same registry, `fauna.account.state.put`.
    crate::offline_gate::declare_wire_kind(&create_button, "fauna.account.state.put");

    let input_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    input_row.append(&input);
    input_row.append(&create_button);
    group.add(&input_row);

    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    set_test_id(&list, ids::PERSONALIZATION_TRAINED_FACTOR_LIST);
    group.add(&list);

    let empty = gtk::Label::new(Some(S::TRAINED_TOPICS_EMPTY));
    empty.add_css_class("dim-label");
    empty.set_halign(gtk::Align::Start);
    group.add(&empty);

    // The publish sheet lives in this group so it reveals in place, directly
    // under the row that opened it (the `admin-dns-rename-sheet` shape).
    let publish = publish_sheet::build_widgets();
    group.add(&publish.root);

    TrainedTopicsWidgets {
        group,
        input,
        create_button,
        list,
        empty,
        publish,
    }
}

/// The wired facet — the home page holds one and calls
/// [`TrainedTopics::refresh`] from its `connect_map` (a settings sub-page that
/// skips refresh-on-visible renders permanently empty).
pub struct TrainedTopics {
    ctx: Rc<Ctx>,
}

/// Wire the facet: connect the create/rename commit paths; loading happens on
/// each [`TrainedTopics::refresh`].
pub fn wire(
    client: &Rc<FaunaClient>,
    widgets: TrainedTopicsWidgets,
    error_label: gtk::Label,
) -> TrainedTopics {
    // Wire the sheet over the same client + page error label. The widget clone
    // is a GTK refcount clone, so both this facet and the sheet drive the very
    // tree `build_widgets` already appended to the group.
    let publish = Rc::new(publish_sheet::wire(
        client,
        widgets.publish.clone(),
        error_label.clone(),
    ));

    let ctx = Rc::new(Ctx {
        nest: client.nest_rpc().clone(),
        rt: client.runtime_handle(),
        w: widgets,
        error_label,
        renaming: RefCell::new(None),
        publish: RefCell::new(Some(publish)),
    });

    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .create_button
            .clone()
            .connect_clicked(move |_| submit(&ctx));
    }
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.input.clone().connect_activate(move |_| submit(&ctx));
    }

    TrainedTopics { ctx }
}

impl TrainedTopics {
    /// Load the registry + per-factor counts.
    pub fn refresh(&self) {
        let ctx = &self.ctx;
        let store = crate::account_runtime::handle_source();
        let nest = ctx.nest.clone();
        let ctx_render = Rc::clone(ctx);
        spawn_with_snapshot(
            &ctx.rt,
            move || async move {
                // Kept: not a single NestClient RPC — the resident-handle
                // branch is a local registry load followed by a factor-count
                // RPC (transport.md § Request lifecycle step 3's note).
                hydrate_with_retry(|| load_rows(store.clone(), nest.clone())).await
            },
            move |result| apply(&ctx_render, result),
        );
    }
}

/// Commit the name-input: a rename when a row retargeted it, else a create.
fn submit(ctx: &Rc<Ctx>) {
    let name = ctx.w.input.text().trim().to_string();
    if name.is_empty() {
        return;
    }
    let renaming = ctx.renaming.borrow().clone();
    let store = crate::account_runtime::handle_source();
    let nest = ctx.nest.clone();
    let ctx_render = Rc::clone(ctx);
    ctx.w.create_button.set_sensitive(false);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            match renaming {
                Some(id) => rename(store, nest, id, name).await,
                None => create(store, nest, name).await,
            }
        },
        move |result| {
            // Any successful commit resets the input to create mode.
            if result.is_ok() {
                *ctx_render.renaming.borrow_mut() = None;
                ctx_render.w.input.set_text("");
                ctx_render
                    .w
                    .create_button
                    .set_label(S::TRAINED_FACTOR_CREATE);
            }
            apply(&ctx_render, result);
        },
    );
}

/// Render a load/mutation outcome (GTK main thread).
fn apply(ctx: &Rc<Ctx>, result: FacetResult) {
    ctx.w.create_button.set_sensitive(true);
    match result {
        Ok(rows) => {
            // An unrelated gesture's success (the engagement toggle folds
            // here too) must not clear a still-relevant publish error out
            // from under the still-open, still-failed sheet — see
            // `PublishSheet::is_open`. While the sheet is open its own
            // lifecycle owns the shared page error label: a submit success
            // clears it, and a fresh re-open retires a stale one.
            let sheet_open = ctx
                .publish
                .borrow()
                .as_ref()
                .is_some_and(|sheet| sheet.is_open());
            if !sheet_open {
                crate::settings::render_error_label(&ctx.error_label, None);
            }
            render_rows(ctx, &rows);
        }
        Err(msg) => crate::settings::render_error_label(&ctx.error_label, Some(&msg)),
    }
}

/// Rebuild the `personalization-trained-factor-item` rows.
fn render_rows(ctx: &Rc<Ctx>, rows: &[TrainedTopicRow]) {
    while let Some(child) = ctx.w.list.first_child() {
        ctx.w.list.remove(&child);
    }
    for row in rows {
        ctx.w.list.append(&build_row(ctx, row));
    }
    ctx.w.empty.set_visible(rows.is_empty());
}

/// One row: name + example count + rename/delete.
fn build_row(ctx: &Rc<Ctx>, factor: &TrainedTopicRow) -> gtk::Box {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    // Expose the factor id's hex on the row (`get_attr(item, "factor")` →
    // `<32-hex>`, i.e. the `topic:<hex>` key minus its prefix — a CSS class
    // can't carry `:`). The registry is sealed, so the e2e has no wire-side
    // way to learn the minted key it must pass to the feed-factor-select
    // `select()`; the same idiom as the admin toggles' `state` attr.
    let key_hex = fauna_core::format::hex_full(&factor.id);
    crate::testid::set_test_attr(&row, "factor", &key_hex);
    set_test_id(&row, ids::PERSONALIZATION_TRAINED_FACTOR_ITEM);

    let name = gtk::Label::new(Some(&factor.name));
    name.set_hexpand(true);
    name.set_halign(gtk::Align::Start);
    set_test_id(&name, ids::PERSONALIZATION_TRAINED_FACTOR_NAME);
    row.append(&name);

    let count = gtk::Label::new(Some(&S::trained_factor_examples(
        &factor.example_count.to_string(),
    )));
    count.add_css_class("dim-label");
    set_test_id(&count, ids::PERSONALIZATION_TRAINED_FACTOR_EXAMPLE_COUNT);
    row.append(&count);

    // The Layer-A opt-in (engagement-cues.md § Layer A): watch/skip engagement
    // cues weak-train this factor only while this is on. Initial state is set
    // BEFORE the notify handler connects, so rendering never echoes a write.
    let engagement_label = gtk::Label::new(Some(S::TRAINED_FACTOR_ENGAGEMENT_TOGGLE));
    engagement_label.add_css_class("dim-label");
    row.append(&engagement_label);
    let engagement_toggle = gtk::Switch::builder()
        .valign(gtk::Align::Center)
        .active(factor.learn_from_engagement)
        .tooltip_text(S::TRAINED_FACTOR_ENGAGEMENT_TOGGLE)
        .build();
    set_test_id(
        &engagement_toggle,
        ids::PERSONALIZATION_TRAINED_FACTOR_ENGAGEMENT_TOGGLE,
    );
    crate::offline_gate::declare_wire_kind(&engagement_toggle, "fauna.account.state.put");
    {
        let ctx = Rc::clone(ctx);
        let id = factor.id.clone();
        engagement_toggle.connect_active_notify(move |sw| {
            let store = crate::account_runtime::handle_source();
            let nest = ctx.nest.clone();
            let id = id.clone();
            let on = sw.is_active();
            let ctx_render = Rc::clone(&ctx);
            spawn_with_snapshot(
                &ctx.rt,
                move || async move { set_engagement(store, nest, id, on).await },
                move |result| apply(&ctx_render, result),
            );
        });
    }
    row.append(&engagement_toggle);

    let rename_btn = gtk::Button::with_label(S::TRAINED_FACTOR_RENAME);
    rename_btn.add_css_class("flat");
    rename_btn.set_valign(gtk::Align::Center);
    set_test_id(
        &rename_btn,
        ids::PERSONALIZATION_TRAINED_FACTOR_RENAME_BUTTON,
    );
    {
        let ctx = Rc::clone(ctx);
        let id = factor.id.clone();
        let current_name = factor.name.clone();
        rename_btn.connect_clicked(move |_| {
            *ctx.renaming.borrow_mut() = Some(id.clone());
            ctx.w.input.set_text(&current_name);
            ctx.w.input.grab_focus();
            ctx.w.create_button.set_label(S::TRAINED_FACTOR_SAVE);
        });
    }
    row.append(&rename_btn);

    // Publish… — open the review-prune sheet against THIS factor
    // (topic-factors.md § Publishing a trained factor; frame D8). Opening only
    // reveals a sheet: nothing leaves the device until the user prunes, names
    // the list, and submits.
    let publish_btn = gtk::Button::with_label(S::TRAINED_FACTOR_PUBLISH);
    publish_btn.add_css_class("flat");
    publish_btn.set_valign(gtk::Align::Center);
    set_test_id(
        &publish_btn,
        ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_BUTTON,
    );
    // Opens the sheet AND scores the corpus (`PublishSheet::open` →
    // `FeedManager::score_corpus_for_factor` → `model_fetch`) — a real read,
    // not bare navigation, the `MailRevealSecret`/`OpenEditFilterForm` reason.
    crate::offline_gate::declare_wire_kind(&publish_btn, "fauna.personalization.model.fetch");
    {
        let ctx = Rc::clone(ctx);
        let id = factor.id.clone();
        // A corrupt (non-16-byte) id has no addressable model — nothing to
        // score, nothing to publish — so the row simply offers no publish.
        let key = factor.factor_key.clone();
        publish_btn.set_sensitive(key.is_some());
        publish_btn.connect_clicked(move |_| {
            let (Some(sheet), Some(key)) = (ctx.publish.borrow().clone(), key.clone()) else {
                return;
            };
            sheet.open(id.clone(), key);
        });
    }
    row.append(&publish_btn);

    let delete_btn = gtk::Button::with_label(crate::i18n::strings::common::DELETE);
    delete_btn.add_css_class("flat");
    delete_btn.set_valign(gtk::Align::Center);
    set_test_id(
        &delete_btn,
        ids::PERSONALIZATION_TRAINED_FACTOR_DELETE_BUTTON,
    );
    crate::offline_gate::declare_wire_kind(&delete_btn, "fauna.personalization.model.delete");
    {
        let ctx = Rc::clone(ctx);
        let id = factor.id.clone();
        delete_btn.connect_clicked(move |_| {
            let store = crate::account_runtime::handle_source();
            let nest = ctx.nest.clone();
            let id = id.clone();
            let ctx_render = Rc::clone(&ctx);
            spawn_with_snapshot(
                &ctx.rt,
                move || async move { delete(store, nest, id).await },
                move |result| apply(&ctx_render, result),
            );
        });
    }
    row.append(&delete_btn);

    row
}

// ── The shared lifecycle ──
//
// The registry↔model-plane sequencing (including the delete pairing and the
// create cap) lives in `fauna_client_personalization::topics` — one
// implementation every app binds to (priority #2). This file is the GTK
// shell over it: build the service, localize its typed errors, render.

fn service(nest: &Nest) -> TopicsService<Nest> {
    TopicsService::new(nest.clone())
}

/// The shared crate's typed errors as user-facing text.
///
/// A thin adapter over [`TrainedTopicsError::localized`], which owns the map:
/// this file and tui's `settings/trained_topics.rs` each used to hold their
/// own copy under a "the one place" comment, and they had drifted — linux's
/// catch-all arm pasted the crate's internal `"config: "` / `"model: "`
/// discriminant prefixes onto text a user reads, where tui and both doors
/// hand back the bare message. The adapter stays only so the ten `map_err`
/// sites below keep reading as a function reference.
fn localize(e: TrainedTopicsError) -> String {
    e.localized()
}

// Each gesture below runs through the shared `preference_surfaces`, the same
// calls tui and the `fauna-ffi` seat make, over the account store
// (`crate::account_runtime::handle_source()` — waited for when the page is
// opened before the assembly lands; `config-dissolution.md` § The `__config`
// dissolution schedule → *The closure order*, steps (1) and (5)). Nothing
// here sequences a gesture.

type Store = fauna_sync_engine::account_runtime::SeatAccountStore;

async fn load_rows(store: Store, nest: Nest) -> FacetResult {
    let svc = service(&nest);
    plane::list_trained_topics(&store, &svc)
        .await
        .map_err(localize)
}

async fn create(store: Store, nest: Nest, name: String) -> FacetResult {
    let svc = service(&nest);
    plane::create_trained_topic(&store, &svc, &name)
        .await
        .map_err(localize)
}

async fn rename(store: Store, nest: Nest, id: Vec<u8>, name: String) -> FacetResult {
    let svc = service(&nest);
    plane::rename_trained_topic(&store, &svc, &id, &name)
        .await
        .map_err(localize)
}

async fn delete(store: Store, nest: Nest, id: Vec<u8>) -> FacetResult {
    let svc = service(&nest);
    plane::delete_trained_topic(&store, &svc, &id)
        .await
        .map_err(localize)
}

async fn set_engagement(store: Store, nest: Nest, id: Vec<u8>, on: bool) -> FacetResult {
    let svc = service(&nest);
    plane::set_trained_topic_engagement(&store, &svc, &id, on)
        .await
        .map_err(localize)
}
