//! The **review-prune sheet** for publishing a trained factor as a tier-3
//! labeler (`docs/goal/behavior/topic-factors.md` § Publishing a trained
//! factor; the frame's D8, user-ratified 2026-07-12; the Model kind
//! 2026-08-13).
//!
//! **This file is a GTK shell, not a lifecycle.** Both halves of either kind's
//! act live in shared Rust (priority #2), and every app binds the same calls:
//! `FeedManager::score_corpus_for_factor` / `scrub_corpus_for_factor` read the
//! corpus, and `fauna_client_personalization::publish::{publish_trained_factor_list,
//! publish_trained_factor_model}` own the whole publish lifecycle (derive the
//! per-factor keypair → resolve the next version off the catalog → build +
//! sign → `fauna.labelers.publish`). What is left here is genuinely platform:
//! reveal the sheet, render the rows, collect what survived the prune,
//! localize the typed errors.
//!
//! **The sheet is single-instance and pre-targeted** (the
//! `admin-dns-rename-sheet` shape, not a modal): built once inside the
//! Trained-topics group, hidden until a row's `publish-button` reveals it
//! against that row's factor.
//!
//! **The two kinds share the sheet but not the review body.** A List shares
//! the posts the factor found (already-public ids, so the review bounds
//! endorsement); a Model shares the word patterns it learned (the vocabulary
//! **is** the disclosure, so the review must cover all of it, with no top-N).
//! They read different corpora through different shared faces and publish
//! through different shared lifecycles. Three decisions inherited from the
//! tui lead leg, not re-decided here:
//!
//! 1. **The kind picker is a RAW-VALUE select over the shared catalog**
//!    (`fauna_core::format::publish_kind_options`) — the option a user picks
//!    and the words they read back are the same [`fauna_core::localized::LocalizedText`]
//!    on all 7 apps.
//! 2. **The row's direction word and doc count are shared faces used by BOTH
//!    the publisher's review and the subscriber's inspect panel**
//!    (`fauna_core::format::{ngram_direction_label, ngram_doc_count_label}`).
//! 3. **Swapping the kind discards the prune and re-reads; the public name
//!    survives.** The two bodies review different objects through different
//!    shared faces, so a carried-over prune would paint one kind's refusal
//!    state over the other's un-read corpus. Re-picking the open kind is a
//!    no-op.
//!
//! **What the user is agreeing to, and why the copy is not decoration.**
//! § Publishing ratifies both copy sets and obliges the sheet to state them —
//! the List's two accepted limitations (the corpus is only what this device
//! loaded; no author attribution) and the Model's three (it generalizes; it
//! discloses ≥3-post patterns including the dislike half; anonymous and
//! explicit-examples-only) plus the corpus-size line that makes the rebuild's
//! drops visible.
//!
//! Prune semantics mirror `restore-kind-checkbox`: every row arrives
//! **checked**, and the user unchecks what they would rather not endorse.

use fauna_ui_ids as ids;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;

use fauna_client_personalization::publish::{
    PublishListError, PublishModelError, REVIEW_TOP_N, publish_trained_factor_list,
    publish_trained_factor_model,
};
use fauna_feed::{ReviewNgram, ScoredExemplar, TrainedModelReview};

use crate::async_helper::spawn_with_snapshot;
use crate::client::FaunaClient;
use crate::i18n::strings::personalization as S;
use crate::testid::set_test_id;

type Nest = Arc<fauna_client::NestClient>;

/// Which artifact kind the sheet is reviewing (tui's `PublishKind` twin).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum PublishKind {
    /// The v1 List — the default, because it is the **weaker disclosure**
    /// (`fauna_core::format::publish_kind_options` owns that ordering rule
    /// for all 7 apps, and the picker paints in its order).
    #[default]
    List,
    /// The v2 `text-model`.
    Model,
}

impl PublishKind {
    /// The `artifact_kind` discriminator this kind publishes as — the value
    /// the raw-value picker round-trips, so a driver names the same two
    /// strings on every app.
    fn wire(self) -> &'static str {
        match self {
            PublishKind::List => fauna_core::scoring::artifact_kind::LIST,
            PublishKind::Model => fauna_core::scoring::artifact_kind::TEXT_MODEL,
        }
    }

    /// Resolve a picked value back to a kind. An unrecognised value keeps the
    /// default (List) rather than panicking — the picker only ever emits its
    /// own two option tokens, so this arm is unreachable from the UI.
    fn from_wire(value: &str) -> Self {
        if value == fauna_core::scoring::artifact_kind::TEXT_MODEL {
            PublishKind::Model
        } else {
            PublishKind::List
        }
    }
}

/// Build the `personalization-trained-factor-publish-kind-select` dropdown
/// over the shared `fauna_core::format::publish_kind_options` catalog.
fn build_publish_kind_select() -> crate::wire_kind_dropdown::WireKindDropdown {
    let values: Vec<String> = fauna_core::format::publish_kind_options()
        .into_iter()
        .map(|o| o.value)
        .collect();
    crate::wire_kind_dropdown::WireKindDropdown::build(
        values,
        ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_KIND_SELECT,
        crate::i18n::publish_kind,
    )
}

/// Static widget handles (built client-free for the ID-conformance test).
///
/// `Clone` is a GTK-refcount clone — every field is a handle to the *same*
/// underlying widget.
#[derive(Clone)]
pub struct PublishSheetWidgets {
    /// The sheet container the Trained-topics group appends; hidden until a
    /// row's publish button reveals it.
    pub root: gtk::Box,
    name_input: gtk::Entry,
    /// Text swaps per kind — set by [`update_body_for_kind`] /
    /// [`refresh_limitation_note`].
    limitation_note: gtk::Label,
    name_label: gtk::Label,
    kind_select: crate::wire_kind_dropdown::WireKindDropdown,
    /// The List review body — title + list + empty state, toggled as one
    /// group.
    exemplar_group: gtk::Box,
    exemplar_list: gtk::ListBox,
    exemplar_empty: gtk::Label,
    /// The Model review body — the exemplar group's twin.
    ngram_group: gtk::Box,
    ngram_list: gtk::ListBox,
    ngram_empty: gtk::Label,
    submit: gtk::Button,
    cancel: gtk::Button,
}

/// Everything the handlers + render need.
struct Ctx {
    nest: Nest,
    secret_hex: String,
    rt: tokio::runtime::Handle,
    w: PublishSheetWidgets,
    /// Page-level `error-message` label (one error element per page, e2e Rule 2).
    error_label: gtk::Label,
    /// The factor the open sheet targets — its 16-byte registry id. `None`
    /// while the sheet is closed.
    target: RefCell<Option<Vec<u8>>>,
    /// The same factor's `topic:<hex>` composition key — what scoring/scrub
    /// addresses the model by. Stored so a kind swap can re-read without the
    /// caller passing it again.
    factor_key: RefCell<Option<String>>,
    /// Which kind the open sheet is reviewing.
    kind: Cell<PublishKind>,
    /// Bumped on every open/close/kind-swap. The corpus read is async (a nest
    /// round trip), so without this a result scored for factor A (or a
    /// stale kind) can land after the sheet moved on and render under the
    /// wrong target — submit would then publish under the wrong identity or
    /// the wrong kind's un-read corpus.
    generation: Cell<u64>,
    /// The List kind's scored exemplars paired with the checkbox that decides
    /// each one's fate.
    rows: RefCell<Vec<(ScoredExemplar, gtk::CheckButton)>>,
    /// The Model kind's scrubbed n-grams, same pairing.
    ngram_rows: RefCell<Vec<(ReviewNgram, gtk::CheckButton)>>,
    /// The Model corpus-size facts, verbatim from the shared read. Do NOT
    /// shrink these on prune — they are the artifact's own class doc
    /// counters (the shared lifecycle's documented rule).
    more_docs: Cell<u32>,
    less_docs: Cell<u32>,
    included_examples: Cell<u32>,
    marked_examples: Cell<u32>,
}

/// Build the sheet's static widget tree — every static ui.yaml ID present, no
/// client dependency.
pub fn build_widgets() -> PublishSheetWidgets {
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    root.set_visible(false);
    set_test_id(&root, ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_SHEET);

    let title = gtk::Label::new(Some(S::PUBLISH_SHEET_TITLE));
    title.add_css_class("title-4");
    title.set_halign(gtk::Align::Start);
    root.append(&title);

    // The kind picker — a RAW-VALUE select whose options come from the shared
    // catalog, so the two option tokens are the same strings on all 7 apps
    // and the words the user reads cannot drift from the row they produce.
    let kind_label = gtk::Label::new(Some(S::PUBLISH_KIND_LABEL));
    kind_label.add_css_class("dim-label");
    kind_label.set_halign(gtk::Align::Start);
    root.append(&kind_label);
    let kind_select = build_publish_kind_select();
    kind_select.dd.set_halign(gtk::Align::Start);
    root.append(&kind_select.dd);

    // The mandated disclosures (§ Publishing) — wrapped, not truncated: a
    // limitation the user cannot read is not stated. Text swaps per kind
    // (`refresh_limitation_note`), so the tree owns a handle rather than
    // static text.
    let limitation_note = gtk::Label::new(Some(S::PUBLISH_LIMITATION_NOTE));
    limitation_note.add_css_class("dim-label");
    limitation_note.set_halign(gtk::Align::Start);
    limitation_note.set_xalign(0.0);
    limitation_note.set_wrap(true);
    set_test_id(
        &limitation_note,
        ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_LIMITATION_NOTE,
    );
    root.append(&limitation_note);

    let name_label = gtk::Label::new(Some(S::PUBLISH_NAME_LABEL));
    name_label.add_css_class("dim-label");
    name_label.set_halign(gtk::Align::Start);
    root.append(&name_label);

    // Starts blank on every open: the sealed registry name is PRIVATE (§
    // Publishing), so prefilling it would leak the user's own label into a
    // public artifact by default — the one thing the two-names split exists to
    // prevent.
    let name_input = gtk::Entry::builder()
        .placeholder_text(S::PUBLISH_NAME_PLACEHOLDER)
        .hexpand(true)
        .build();
    set_test_id(
        &name_input,
        ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_NAME_INPUT,
    );
    root.append(&name_input);

    // The List kind's review body — the scored top-N exemplars.
    let exemplar_group = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .build();
    let exemplars_title = gtk::Label::new(Some(S::PUBLISH_EXEMPLARS_TITLE));
    exemplars_title.add_css_class("dim-label");
    exemplars_title.set_halign(gtk::Align::Start);
    exemplar_group.append(&exemplars_title);

    let exemplar_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    set_test_id(
        &exemplar_list,
        ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_EXEMPLAR_LIST,
    );
    exemplar_group.append(&exemplar_list);

    let exemplar_empty = gtk::Label::new(Some(S::PUBLISH_EXEMPLARS_EMPTY));
    exemplar_empty.add_css_class("dim-label");
    exemplar_empty.set_halign(gtk::Align::Start);
    exemplar_empty.set_xalign(0.0);
    exemplar_empty.set_wrap(true);
    set_test_id(
        &exemplar_empty,
        ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_EXEMPLAR_EMPTY,
    );
    exemplar_group.append(&exemplar_empty);
    root.append(&exemplar_group);

    // The Model kind's review body — **every** surviving n-gram. Where the
    // List's exemplar rows bound endorsement of already-public ids, these
    // rows ARE the disclosure, so there is no top-N here. Starts hidden — the
    // sheet always opens on List (the default kind).
    let ngram_group = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .visible(false)
        .build();
    let ngrams_title = gtk::Label::new(Some(S::PUBLISH_NGRAMS_TITLE));
    ngrams_title.add_css_class("dim-label");
    ngrams_title.set_halign(gtk::Align::Start);
    ngram_group.append(&ngrams_title);

    let ngram_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    set_test_id(
        &ngram_list,
        ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_NGRAM_LIST,
    );
    ngram_group.append(&ngram_list);

    let ngram_empty = gtk::Label::new(Some(S::PUBLISH_NGRAMS_EMPTY));
    ngram_empty.add_css_class("dim-label");
    ngram_empty.set_halign(gtk::Align::Start);
    ngram_empty.set_xalign(0.0);
    ngram_empty.set_wrap(true);
    set_test_id(
        &ngram_empty,
        ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_NGRAM_EMPTY,
    );
    ngram_group.append(&ngram_empty);
    root.append(&ngram_group);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);

    let submit = gtk::Button::with_label(S::PUBLISH_SUBMIT);
    submit.add_css_class("suggested-action");
    set_test_id(
        &submit,
        ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_SUBMIT_BUTTON,
    );
    crate::offline_gate::declare_wire_kind(&submit, "fauna.labelers.publish");
    actions.append(&submit);

    let cancel = gtk::Button::with_label(crate::i18n::strings::common::CANCEL);
    set_test_id(
        &cancel,
        ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_CANCEL_BUTTON,
    );
    actions.append(&cancel);

    root.append(&actions);

    PublishSheetWidgets {
        root,
        name_input,
        limitation_note,
        name_label,
        kind_select,
        exemplar_group,
        exemplar_list,
        exemplar_empty,
        ngram_group,
        ngram_list,
        ngram_empty,
        submit,
        cancel,
    }
}

/// The wired sheet — the Trained-topics facet holds one and opens it from a
/// row's publish button.
pub struct PublishSheet {
    ctx: Rc<Ctx>,
}

/// Wire the sheet: connect submit/cancel/kind-select. Opening happens per row.
pub fn wire(
    client: &Rc<FaunaClient>,
    widgets: PublishSheetWidgets,
    error_label: gtk::Label,
) -> PublishSheet {
    let ctx = Rc::new(Ctx {
        nest: client.nest_rpc().clone(),
        secret_hex: client.secret_hex().to_string(),
        rt: client.runtime_handle(),
        w: widgets,
        error_label,
        target: RefCell::new(None),
        factor_key: RefCell::new(None),
        kind: Cell::new(PublishKind::List),
        generation: Cell::new(0),
        rows: RefCell::new(Vec::new()),
        ngram_rows: RefCell::new(Vec::new()),
        more_docs: Cell::new(0),
        less_docs: Cell::new(0),
        included_examples: Cell::new(0),
        marked_examples: Cell::new(0),
    });

    {
        let ctx = Rc::clone(&ctx);
        ctx.w.submit.clone().connect_clicked(move |_| submit(&ctx));
    }
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.cancel.clone().connect_clicked(move |_| close(&ctx));
    }
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .kind_select
            .dd
            .clone()
            .connect_selected_notify(move |_| {
                let kind = PublishKind::from_wire(
                    ctx.w
                        .kind_select
                        .selected_kind(fauna_core::scoring::artifact_kind::LIST),
                );
                set_kind(&ctx, kind);
            });
    }

    PublishSheet { ctx }
}

impl PublishSheet {
    /// Whether the sheet is revealed — what the facet's success fold consults
    /// before clearing the page error, so an unrelated gesture's success never
    /// clears a still-relevant publish error out from under the still-open,
    /// still-failed sheet (the clobber windows fixed in its render precedence
    /// chain 2026-07-18, pinned cross-app by test_trained_topics.py's two
    /// publish-error cases).
    pub fn is_open(&self) -> bool {
        self.ctx.w.root.get_visible()
    }

    /// Reveal the sheet against one factor and score its List corpus (the
    /// sheet always opens on the List kind — the weaker disclosure default).
    ///
    /// `factor_id` is the registry's 16-byte id (what publishing derives the
    /// signing key from); `factor_key` is the same id's `topic:<hex>`
    /// composition key (what scoring/scrub addresses the model by). Both come
    /// off the row — the sheet never re-derives either.
    pub fn open(&self, factor_id: Vec<u8>, factor_key: String) {
        let ctx = &self.ctx;
        // A fresh, user-initiated open starts a new attempt: retire a stale
        // error from a prior failed publish immediately, before the new corpus
        // score even lands (the reopen half of the same contract `is_open`
        // guards the other half of).
        crate::settings::render_error_label(&ctx.error_label, None);
        *ctx.target.borrow_mut() = Some(factor_id);
        *ctx.factor_key.borrow_mut() = Some(factor_key.clone());
        ctx.kind.set(PublishKind::List);
        ctx.more_docs.set(0);
        ctx.less_docs.set(0);
        ctx.included_examples.set(0);
        ctx.marked_examples.set(0);
        // Invalidate any corpus read still in flight for a previous open — its
        // rows belong to another factor (see `Ctx::generation`).
        let generation = ctx.generation.get() + 1;
        ctx.generation.set(generation);
        ctx.rows.borrow_mut().clear();
        ctx.ngram_rows.borrow_mut().clear();
        clear_rows(ctx);
        clear_ngram_rows(ctx);
        ctx.w.name_input.set_text("");
        ctx.w.exemplar_empty.set_visible(false);
        ctx.w.ngram_empty.set_visible(false);
        // A `set_selected` to the value already selected is a no-op (no
        // `notify::selected`); when it DOES fire, `set_kind`'s guard is
        // already satisfied (`ctx.kind` was set above), so this can never
        // double-fetch.
        ctx.w.kind_select.select_kind(PublishKind::List.wire());
        update_body_for_kind(ctx, PublishKind::List);
        ctx.w.submit.set_sensitive(false);
        ctx.w.root.set_visible(true);
        ctx.w.name_input.grab_focus();

        // The corpus is the loaded feed window, so it is the live `FeedManager`
        // singleton's — the same instance the Feed page renders from. A second
        // manager would score an empty window and silently publish nothing.
        let Some(manager) = crate::feed::host::manager() else {
            crate::settings::render_error_label(
                &ctx.error_label,
                Some(crate::i18n::strings::common::NOT_CONNECTED),
            );
            return;
        };
        let ctx_render = Rc::clone(ctx);
        spawn_with_snapshot(
            &ctx.rt,
            move || async move {
                manager
                    .score_corpus_for_factor(&factor_key, REVIEW_TOP_N)
                    .await
            },
            move |res| {
                if ctx_render.generation.get() == generation {
                    render_exemplars(&ctx_render, res);
                }
            },
        );
    }
}

/// `-publish-kind-select`: swap which artifact kind the open sheet reviews.
///
/// **Re-reads from scratch.** The two kinds review different objects through
/// different shared faces, so the prune and the corpus-size facts all belong
/// to the kind that produced them. Picking the kind already selected is a
/// no-op. The public name survives — it is the user's own words about the
/// factor and equally true of either kind.
fn set_kind(ctx: &Rc<Ctx>, kind: PublishKind) {
    if ctx.kind.get() == kind {
        return;
    }
    if ctx.target.borrow().is_none() {
        return;
    }
    let Some(factor_key) = ctx.factor_key.borrow().clone() else {
        return;
    };
    ctx.kind.set(kind);
    ctx.more_docs.set(0);
    ctx.less_docs.set(0);
    ctx.included_examples.set(0);
    ctx.marked_examples.set(0);
    let generation = ctx.generation.get() + 1;
    ctx.generation.set(generation);
    ctx.rows.borrow_mut().clear();
    ctx.ngram_rows.borrow_mut().clear();
    clear_rows(ctx);
    clear_ngram_rows(ctx);
    ctx.w.exemplar_empty.set_visible(false);
    ctx.w.ngram_empty.set_visible(false);
    update_body_for_kind(ctx, kind);
    ctx.w.submit.set_sensitive(false);

    let Some(manager) = crate::feed::host::manager() else {
        crate::settings::render_error_label(
            &ctx.error_label,
            Some(crate::i18n::strings::common::NOT_CONNECTED),
        );
        return;
    };
    let ctx_render = Rc::clone(ctx);
    match kind {
        PublishKind::List => {
            spawn_with_snapshot(
                &ctx.rt,
                move || async move {
                    manager
                        .score_corpus_for_factor(&factor_key, REVIEW_TOP_N)
                        .await
                },
                move |res| {
                    if ctx_render.generation.get() == generation {
                        render_exemplars(&ctx_render, res);
                    }
                },
            );
        }
        PublishKind::Model => {
            spawn_with_snapshot(
                &ctx.rt,
                move || async move { manager.scrub_corpus_for_factor(&factor_key).await },
                move |res| {
                    if ctx_render.generation.get() == generation {
                        render_ngrams(&ctx_render, res);
                    }
                },
            );
        }
    }
}

/// Swap the sheet's per-kind chrome: which review group is visible, and the
/// name label (§ Publishing owns both copy sets).
fn update_body_for_kind(ctx: &Rc<Ctx>, kind: PublishKind) {
    let model = kind == PublishKind::Model;
    ctx.w.exemplar_group.set_visible(!model);
    ctx.w.ngram_group.set_visible(model);
    ctx.w.name_label.set_text(if model {
        S::PUBLISH_NAME_LABEL_MODEL
    } else {
        S::PUBLISH_NAME_LABEL
    });
    refresh_limitation_note(ctx);
}

/// The mandated copy (§ Publishing), recomputed whenever the corpus counts
/// change (not only on kind switch, since the Model's counts arrive async).
/// The List states its two accepted limitations; the Model states its three
/// plus the corpus-size line that makes the rebuild's drops visible.
fn refresh_limitation_note(ctx: &Rc<Ctx>) {
    let text = if ctx.kind.get() == PublishKind::Model {
        format!(
            "{}\n{}",
            S::PUBLISH_LIMITATION_NOTE_MODEL,
            S::publish_corpus_size(
                &ctx.included_examples.get().to_string(),
                &ctx.marked_examples.get().to_string(),
            )
        )
    } else {
        S::PUBLISH_LIMITATION_NOTE.to_string()
    };
    ctx.w.limitation_note.set_text(&text);
}

/// Close without publishing: drop the target + the reviewed rows, so a
/// re-open cannot inherit a stale prune.
fn close(ctx: &Rc<Ctx>) {
    *ctx.target.borrow_mut() = None;
    *ctx.factor_key.borrow_mut() = None;
    ctx.generation.set(ctx.generation.get() + 1);
    ctx.rows.borrow_mut().clear();
    ctx.ngram_rows.borrow_mut().clear();
    clear_rows(ctx);
    clear_ngram_rows(ctx);
    ctx.w.root.set_visible(false);
}

fn clear_rows(ctx: &Rc<Ctx>) {
    while let Some(child) = ctx.w.exemplar_list.first_child() {
        ctx.w.exemplar_list.remove(&child);
    }
}

fn clear_ngram_rows(ctx: &Rc<Ctx>) {
    while let Some(child) = ctx.w.ngram_list.first_child() {
        ctx.w.ngram_list.remove(&child);
    }
}

/// Render the scored List corpus (GTK main thread).
fn render_exemplars(ctx: &Rc<Ctx>, res: Result<Vec<ScoredExemplar>, String>) {
    let exemplars = match res {
        Ok(e) => e,
        Err(msg) => {
            crate::settings::render_error_label(&ctx.error_label, Some(&msg));
            return;
        }
    };
    crate::settings::render_error_label(&ctx.error_label, None);
    clear_rows(ctx);
    ctx.rows.borrow_mut().clear();

    for exemplar in exemplars {
        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .accessible_role(gtk::AccessibleRole::Group)
            .build();
        set_test_id(
            &row,
            ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_EXEMPLAR_ITEM,
        );

        // Default-checked = include (the `restore-kind-checkbox` shape).
        let include = gtk::CheckButton::new();
        include.set_active(true);
        include.set_valign(gtk::Align::Center);
        set_test_id(
            &include,
            ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_EXEMPLAR_CHECKBOX,
        );
        // Connected AFTER the initial `set_active`, so building a row never
        // echoes a toggle into the sensitivity read below.
        {
            let ctx = Rc::clone(ctx);
            include.connect_toggled(move |_| refresh_submit_sensitivity(&ctx));
        }
        row.append(&include);

        let preview = gtk::Label::new(Some(&exemplar.preview));
        preview.set_hexpand(true);
        preview.set_halign(gtk::Align::Start);
        preview.set_xalign(0.0);
        preview.set_ellipsize(gtk::pango::EllipsizeMode::End);
        set_test_id(
            &preview,
            ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_EXEMPLAR_TEXT,
        );
        row.append(&preview);

        // The per-mille the artifact will carry verbatim — the same number a
        // subscriber reads at inspect, with no rescale between here and there.
        let score = gtk::Label::new(Some(&S::publish_score(&exemplar.score.to_string())));
        score.add_css_class("dim-label");
        set_test_id(
            &score,
            ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_EXEMPLAR_SCORE,
        );
        row.append(&score);

        ctx.w.exemplar_list.append(&row);
        ctx.rows.borrow_mut().push((exemplar, include));
    }

    ctx.w
        .exemplar_empty
        .set_visible(ctx.rows.borrow().is_empty());
    refresh_submit_sensitivity(ctx);
}

/// Render a Model corpus scrub (GTK main thread) — the `render_exemplars`
/// twin for the Model review body.
fn render_ngrams(ctx: &Rc<Ctx>, res: Result<TrainedModelReview, String>) {
    let review = match res {
        Ok(r) => r,
        Err(msg) => {
            crate::settings::render_error_label(&ctx.error_label, Some(&msg));
            return;
        }
    };
    crate::settings::render_error_label(&ctx.error_label, None);
    // `more_docs`/`less_docs` go over UNSHRUNK by the prune (the shared
    // lifecycle's documented rule) — recorded here, and only recomputed by
    // another read, never by the checkbox toggles below.
    ctx.more_docs.set(review.more_docs);
    ctx.less_docs.set(review.less_docs);
    ctx.included_examples.set(review.included_examples);
    ctx.marked_examples.set(review.marked_examples);
    refresh_limitation_note(ctx);
    clear_ngram_rows(ctx);
    ctx.ngram_rows.borrow_mut().clear();

    for ngram in review.ngrams {
        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .accessible_role(gtk::AccessibleRole::Group)
            .build();
        set_test_id(&row, ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_NGRAM_ITEM);

        let include = gtk::CheckButton::new();
        include.set_active(true);
        include.set_valign(gtk::Align::Center);
        set_test_id(
            &include,
            ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_NGRAM_CHECKBOX,
        );
        {
            let ctx = Rc::clone(ctx);
            include.connect_toggled(move |_| refresh_submit_sensitivity(&ctx));
        }
        row.append(&include);

        let text = gtk::Label::new(Some(&ngram.ngram));
        text.set_hexpand(true);
        text.set_halign(gtk::Align::Start);
        text.set_xalign(0.0);
        text.set_ellipsize(gtk::pango::EllipsizeMode::End);
        set_test_id(
            &text,
            ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_NGRAM_TEXT,
        );
        row.append(&text);

        // Direction and count are BOTH shared faces, and both for the same
        // reason: the publisher's review is a promise about what a
        // subscriber reads back at `labeler-inspect-model-entry-*`.
        let direction =
            gtk::Label::new(Some(&crate::i18n::ngram_direction(ngram.more, ngram.less)));
        direction.add_css_class("dim-label");
        set_test_id(
            &direction,
            ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_NGRAM_DIRECTION,
        );
        row.append(&direction);

        let count = gtk::Label::new(Some(&crate::i18n::ngram_doc_count(ngram.more, ngram.less)));
        count.add_css_class("dim-label");
        set_test_id(
            &count,
            ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_NGRAM_COUNT,
        );
        row.append(&count);

        ctx.w.ngram_list.append(&row);
        ctx.ngram_rows.borrow_mut().push((ngram, include));
    }

    ctx.w
        .ngram_empty
        .set_visible(ctx.ngram_rows.borrow().is_empty());
    refresh_submit_sensitivity(ctx);
}

/// Publishing nothing is not a thing the artifact means, so the button says so
/// by going insensitive rather than by no-op'ing a click — a click that does
/// nothing and explains nothing is indistinguishable from a broken sheet (the
/// `restore-confirm-button` precedent, which starts insensitive for the same
/// reason). Covers both "the corpus scored/scrubbed nothing" and "the user
/// unchecked everything" — for whichever kind is currently open.
fn refresh_submit_sensitivity(ctx: &Rc<Ctx>) {
    let any_kept = match ctx.kind.get() {
        PublishKind::List => ctx
            .rows
            .borrow()
            .iter()
            .any(|(_, include)| include.is_active()),
        PublishKind::Model => ctx
            .ngram_rows
            .borrow()
            .iter()
            .any(|(_, include)| include.is_active()),
    };
    ctx.w.submit.set_sensitive(any_kept);
}

/// Publish what survived the prune, through whichever kind's lifecycle is
/// open.
fn submit(ctx: &Rc<Ctx>) {
    // A blank name is refused here rather than at the wire — the same guard the
    // sibling create/rename flow uses (`trained_topics::submit`), so a user who
    // left the box empty gets nothing to dismiss.
    let name = ctx.w.name_input.text().trim().to_string();
    if name.is_empty() {
        ctx.w.name_input.grab_focus();
        return;
    }
    let Some(factor_id) = ctx.target.borrow().clone() else {
        return;
    };
    let Ok(id16) = <[u8; 16]>::try_from(factor_id.as_slice()) else {
        // A non-16-byte registry id has no addressable model, so it cannot have
        // been scored either — unreachable from a rendered row.
        return;
    };
    let secret = match fauna_core::hex32::decode(&ctx.secret_hex) {
        Ok(s) => s,
        Err(e) => {
            crate::settings::render_error_label(
                &ctx.error_label,
                Some(&format!("decode secret: {e}")),
            );
            return;
        }
    };
    let nest = ctx.nest.clone();
    let ctx_render = Rc::clone(ctx);

    match ctx.kind.get() {
        PublishKind::List => {
            let entries: Vec<(String, i64)> = ctx
                .rows
                .borrow()
                .iter()
                .filter(|(_, include)| include.is_active())
                .map(|(e, _)| (e.post_id.clone(), e.score))
                .collect();
            if entries.is_empty() {
                // Unreachable through the button (insensitive with nothing
                // kept); kept so the invariant holds at the call too.
                return;
            }
            ctx.w.submit.set_sensitive(false);
            spawn_with_snapshot(
                &ctx.rt,
                move || async move {
                    publish_trained_factor_list(nest, &secret, &id16, &name, entries).await
                },
                move |res| apply(&ctx_render, res.map(|_| ()).map_err(localize_list)),
            );
        }
        PublishKind::Model => {
            let ngrams: Vec<(String, u32, u32)> = ctx
                .ngram_rows
                .borrow()
                .iter()
                .filter(|(_, include)| include.is_active())
                .map(|(n, _)| (n.ngram.clone(), n.more, n.less))
                .collect();
            if ngrams.is_empty() {
                return;
            }
            // ⚠ `more_docs`/`less_docs` go over UNSHRUNK by the prune — a shell
            // must not "helpfully" recount from the kept rows (the shared
            // lifecycle's documented rule; `render_ngrams`'s comment).
            let more_docs = ctx.more_docs.get();
            let less_docs = ctx.less_docs.get();
            ctx.w.submit.set_sensitive(false);
            spawn_with_snapshot(
                &ctx.rt,
                move || async move {
                    publish_trained_factor_model(
                        nest, &secret, &id16, &name, more_docs, less_docs, ngrams,
                    )
                    .await
                },
                move |res| apply(&ctx_render, res.map(|_| ()).map_err(localize_model)),
            );
        }
    }
}

/// Render a publish outcome: success closes the sheet, a refusal leaves it open
/// with the reason on the page `error-message` — the user's prune survives, so
/// they can fix the name and retry without reviewing everything again. Shared
/// by both kinds — the caller already localized the typed error into `Err`.
fn apply(ctx: &Rc<Ctx>, res: Result<(), String>) {
    ctx.w.submit.set_sensitive(true);
    match res {
        Ok(()) => {
            crate::settings::render_error_label(&ctx.error_label, None);
            close(ctx);
        }
        Err(msg) => crate::settings::render_error_label(&ctx.error_label, Some(&msg)),
    }
}

/// The one place the shared crate's typed List publish errors become
/// user-facing text. Every variant is already a complete sentence naming the
/// offending row where it has one, so only a variant needing a *number the
/// crate cannot know* would be special-cased, and none does.
fn localize_list(e: PublishListError) -> String {
    e.to_string()
}

/// The Model lifecycle's error twin — same passthrough rule, same reason.
fn localize_model(e: PublishModelError) -> String {
    e.to_string()
}
