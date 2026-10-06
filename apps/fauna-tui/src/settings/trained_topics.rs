//! The Personalization home's **Trained topics** facet — the user's trainable
//! tier-1 topic factors (`docs/goal/behavior/topic-factors.md` § Authoring
//! surface & picker; the ID families were user-approved 2026-07-09, the
//! per-row engagement toggle 2026-07-15).
//!
//! A person creates a topic here ("Cats"), then teaches it from the feed's
//! per-post *more like this* / *less like this* gestures
//! (`crate::feed`); the factor composes into a feed through the create-feed
//! picker's `topic:<hex>` option. The model is sealed under the user's own key
//! — the nest stores an opaque blob and never learns what the topic means.
//!
//! **No machine, and no FFI hop.** The whole lifecycle is already shared Rust:
//! `fauna_client_personalization::topics::TrainedTopics` owns the four gestures
//! *and* the registry↔model-plane sequencing they span (the create cap, the
//! advisory example-count read, and the delete's registry-removal-then-
//! `model.delete` pairing). tui calls it natively, the way linux does, so this
//! module is a dumb renderer plus the op plumbing — priority #2: no client
//! re-derives the pairing.
//!
//! **Row scoping is MIXED here, and that is the e2e action file's call, not a
//! house style.** `actions/personalization.py` reads `-name` / `-example-count`
//! and clicks `-rename-button` / `-delete-button` with a plain `index=`, so
//! those paint **FLAT**; but `engagement_toggle_on` reads the toggle with a
//! single-step `scope="personalization-trained-factor-item[i]"` (while
//! `set_engagement_toggle` *clicks* it flat), so the toggle paints
//! `.within(ids::PERSONALIZATION_TRAINED_FACTOR_ITEM, i)` — which satisfies both,
//! because one toggle per row makes the flat occurrence index equal the row
//! index. Registering the toggle flat would leave every scoped read empty while
//! the page painted perfectly.

use fauna_client_personalization::publish::{PublishListError, PublishModelError};
use fauna_client_personalization::{TrainedTopicRow, TrainedTopics, TrainedTopicsError};
use fauna_feed::ScoredExemplar;
use fauna_i18n::strings::personalization as p;
use fauna_ui_ids as ids;

use super::{Action, SettingsField, SettingsState};
use crate::element::{Element, Field, Gesture};

/// Which artifact kind the sheet is reviewing (`topic-factors.md` § Publishing
/// a trained factor: v1 List, v2 `text-model`).
///
/// The two kinds share the *sheet* but not the *review body*: a List shares the
/// posts the factor found (already-public ids, so the review bounds
/// endorsement), a Model shares the word patterns it learned (the vocabulary
/// **is** the disclosure, so the review must cover all of it). They therefore
/// read different corpora through different shared faces and publish through
/// different shared lifecycles — which is why this is a kind discriminator, not
/// a rendering flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum PublishKind {
    /// The v1 List — the default, because it is the **weaker disclosure**
    /// (`fauna_core::format::publish_kind_options` owns that ordering rule for
    /// all 7 apps, and the picker paints in its order).
    #[default]
    List,
    /// The v2 `text-model`.
    Model,
}

impl PublishKind {
    /// The `artifact_kind` discriminator this kind publishes as — the value the
    /// raw-value picker round-trips, so a driver names the same two strings on
    /// every app and no translation can make the select undriveable
    /// (`backup-destination-kind-select`'s rule).
    pub(crate) fn wire(self) -> &'static str {
        match self {
            PublishKind::List => fauna_core::scoring::artifact_kind::LIST,
            PublishKind::Model => fauna_core::scoring::artifact_kind::TEXT_MODEL,
        }
    }

    /// Resolve a picked value back to a kind. An unrecognised value keeps the
    /// **default** rather than panicking — the picker only ever emits its own
    /// two option tokens, so this arm is unreachable from the UI, and falling
    /// back to List means an impossible input cannot silently upgrade a
    /// publish to the *stronger* disclosure.
    pub(crate) fn from_wire(value: &str) -> Self {
        if value == fauna_core::scoring::artifact_kind::TEXT_MODEL {
            PublishKind::Model
        } else {
            PublishKind::List
        }
    }
}

/// The review-prune sheet's state (`topic-factors.md` § Publishing a trained
/// factor; frame D8, user-ratified 2026-07-12; the Model kind 2026-08-13).
///
/// **Single-instance and pre-targeted** — the `admin-dns-rename-sheet` idiom
/// linux set for this surface: one sheet, revealed against the row whose
/// `-publish-button` opened it, rather than one sheet per row. `target` being
/// `Some` *is* "the sheet is open"; there is no separate visibility flag to
/// drift from it.
#[derive(Debug, Clone, Default)]
pub(crate) struct PublishSheetState {
    /// Which kind the sheet is reviewing. Switching it **discards the prune and
    /// re-reads**: the two bodies review different objects entirely, so a
    /// carried-over `scored` would paint one kind's refusal state over the
    /// other kind's un-read corpus.
    pub(crate) kind: PublishKind,
    /// The scrubbed vocabulary, most-informative first, each paired with
    /// whether it survives the prune — the Model twin of [`Self::rows`]. **All**
    /// survivors are listed, never a top-N: the vocabulary is the disclosure,
    /// so the review has to cover everything that crosses.
    pub(crate) ngrams: Vec<(fauna_feed::ReviewNgram, bool)>,
    /// The corpus-size facts the Model's mandated copy states, verbatim from
    /// the shared read. `more_docs`/`less_docs` are the artifact's own class
    /// doc-counters and **do not shrink when the user prunes** (the shared
    /// lifecycle's documented rule: they are the posterior's priors and the
    /// damp's sample count, so shrinking them would make a published model look
    /// more confident than it is).
    pub(crate) more_docs: u32,
    pub(crate) less_docs: u32,
    /// The N and M of "built from N public examples of your M marked posts".
    /// `M - N` is what the rebuild dropped as restricted, deleted, or
    /// unfetchable — visible rather than silent.
    pub(crate) included_examples: u32,
    pub(crate) marked_examples: u32,
    /// The targeted factor's 16-byte registry id — what the publish derives its
    /// per-factor signing key from. `None` while the sheet is closed, which is
    /// also what stops the sheet painting at all.
    pub(crate) target: Option<Vec<u8>>,
    /// The **public** name buffer. Starts BLANK on every open and is never
    /// prefilled from the registry: the sealed name is private, and prefilling
    /// would leak the user's own label into a public artifact by default — the
    /// one thing the two-names split exists to prevent (`topic-factors.md:102`).
    pub(crate) name: String,
    /// The scored corpus, best-first, each paired with whether it survives the
    /// prune. The pairing is the sheet's whole state: submit reads it, nothing
    /// else does (linux's `rows: Vec<(ScoredExemplar, CheckButton)>`, with the
    /// checkbox's `is_active` replaced by a bool tui owns).
    pub(crate) rows: Vec<(ScoredExemplar, bool)>,
    /// Whether the corpus read **for the current kind** has resolved. Distinct
    /// from `rows.is_empty()`: "not scored yet" and "scored nothing" are
    /// different pictures, and only the second one may paint
    /// `-publish-exemplar-empty` / `-publish-ngram-empty` (conflating them
    /// would tell a user the factor scored nothing while the read was still in
    /// flight — the audit slice's "no pass completed is not an empty picture"
    /// finding, in miniature).
    pub(crate) scored: bool,
    /// Set while the publish round trip is in flight — disarms submit so a
    /// double activation cannot publish twice.
    pub(crate) busy: bool,
}

impl PublishSheetState {
    /// Whether the sheet is revealed.
    pub(crate) fn is_open(&self) -> bool {
        self.target.is_some()
    }

    /// The entries that survived the prune, in the shared publish call's
    /// `(hex post_id, per-mille score)` shape. `build_list_artifact` owns the
    /// sort/dedup/validate, so this deliberately hands them over in render
    /// order rather than re-deriving a canonical form no client should own.
    fn kept(&self) -> Vec<(String, i64)> {
        self.rows
            .iter()
            .filter(|(_, keep)| *keep)
            .map(|(e, _)| (e.post_id.clone(), e.score))
            .collect()
    }

    /// The n-grams that survived the prune, in the shared publish call's
    /// `(ngram, more, less)` shape. `build_text_model_artifact` owns the
    /// sort/dedup/validate, so — as with [`Self::kept`] — these go over in
    /// render order rather than re-deriving a canonical form no client owns.
    fn kept_ngrams(&self) -> Vec<(String, u32, u32)> {
        self.ngrams
            .iter()
            .filter(|(_, keep)| *keep)
            .map(|(n, _)| (n.ngram.clone(), n.more, n.less))
            .collect()
    }

    /// Whether **anything** survives the prune — what arms the submit button.
    /// Reads the body the current kind actually publishes: an un-switched
    /// List prune must not arm a Model submit that would send nothing.
    fn any_kept(&self) -> bool {
        match self.kind {
            PublishKind::List => self.rows.iter().any(|(_, keep)| *keep),
            PublishKind::Model => self.ngrams.iter().any(|(_, keep)| *keep),
        }
    }
}

/// The facet's state — the persisted rows plus the one shared name buffer.
#[derive(Debug, Clone, Default)]
pub(crate) struct TrainedTopicsState {
    /// The registry's factors, freshest-listed-last as the shared service
    /// returns them. Empty pre-auth and until the first load resolves.
    pub(crate) rows: Vec<TrainedTopicRow>,
    /// The `personalization-trained-factor-name-input` buffer — ONE input
    /// serving both the create and rename flows (ui.yaml lists a single
    /// `-name-input` for the page; linux/windows/web/apple/android all share
    /// it the same way).
    pub(crate) input: String,
    /// The factor id a rename is retargeting, or `None` while the input is
    /// serving a create. Set by `-rename-button`, cleared on commit.
    pub(crate) renaming: Option<Vec<u8>>,
    /// Set while a list/mutation round trip is in flight — disables the
    /// commit button so a double activation cannot race two
    /// read-modify-write cycles against the same sealed registry
    /// (`muted_words`' `busy`, and linux's `set_sensitive(false)`).
    pub(crate) busy: bool,
    /// The single-instance review-prune sheet (`topic-factors.md` § Publishing).
    pub(crate) publish: PublishSheetState,
    /// Monotonic dispatch counter, bumped every time a load OR a gesture is
    /// dispatched (`trained_topics::load_op` / `gesture_op`) and carried by
    /// its `Op`/`Outcome` as `seq`. `busy` alone only serializes GESTURE
    /// clicks against each other (the buttons disable while `busy`); the
    /// nav-edge/login-time load is dispatched unconditionally and never arms
    /// `busy`, so without this counter it can resolve AFTER a faster
    /// in-flight gesture and silently overwrite fresher rows with stale ones
    /// (a login-time re-load raced a test's own create/delete and
    /// intermittently reintroduced a just-deleted topic, or discarded a
    /// freshly created one).
    pub(crate) dispatch_seq: u64,
    /// The `dispatch_seq` of the last outcome `apply_trained_topics` (or the
    /// failure arm) accepted — the comparison `dispatch_seq` is checked
    /// against before any producer's result is allowed to touch `rows`.
    pub(crate) applied_seq: u64,
}

impl TrainedTopicsState {
    /// Reset the page-local draft on a fresh visit. The persisted `rows`
    /// survive — they are re-read by the nav-edge load, and a stale render is
    /// better than a blank one while it resolves.
    ///
    /// The publish sheet is **closed** here rather than preserved: it is a
    /// review of one factor's corpus, and a prune the user left half-finished
    /// three navigations ago is not a draft worth restoring — it is a set of
    /// public endorsements they would be re-shown out of context.
    pub(super) fn reset_form(&mut self) {
        self.input.clear();
        self.renaming = None;
        self.busy = false;
        self.publish = PublishSheetState::default();
    }

    /// The row at `index`, if the facet has one — the gate every indexed
    /// gesture goes through, so a click landing after a concurrent delete is a
    /// no-op instead of a panic.
    fn row(&self, index: usize) -> Option<&TrainedTopicRow> {
        self.rows.get(index)
    }
}

/// Which shared-service gesture an [`super::Op::TrainedTopicsGesture`] carries.
///
/// One op for all four because the service's contract is identical in each
/// case — await the call, which returns the **fresh row list** on success — so
/// there is exactly one fold and exactly one place the error can bridge onto
/// `error-message`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrainedTopicsGesture {
    Create(String),
    Rename { id: Vec<u8>, name: String },
    Delete(Vec<u8>),
    SetEngagement { id: Vec<u8>, on: bool },
}

impl TrainedTopicsGesture {
    /// Run this gesture over the account store, through the shared
    /// `preference_surfaces` — the same calls linux and the `fauna-ffi` seat
    /// make. The sequence is not written here; this match only names the
    /// gesture.
    pub(super) async fn run(
        self,
        store: &fauna_sync_engine::account_runtime::SeatAccountStore,
        svc: &TrainedTopics<std::sync::Arc<fauna_client::NestClient>>,
    ) -> Result<Vec<TrainedTopicRow>, TrainedTopicsError> {
        use fauna_sync_engine::preference_surfaces as plane;
        match self {
            TrainedTopicsGesture::Create(name) => {
                plane::create_trained_topic(store, svc, &name).await
            }
            TrainedTopicsGesture::Rename { id, name } => {
                plane::rename_trained_topic(store, svc, &id, &name).await
            }
            TrainedTopicsGesture::Delete(id) => plane::delete_trained_topic(store, svc, &id).await,
            TrainedTopicsGesture::SetEngagement { id, on } => {
                plane::set_trained_topic_engagement(store, svc, &id, on).await
            }
        }
    }
}

/// The facet's row list —
/// [`fauna_sync_engine::preference_surfaces::list_trained_topics`].
pub(super) async fn load_rows(
    store: &fauna_sync_engine::account_runtime::SeatAccountStore,
    svc: &TrainedTopics<std::sync::Arc<fauna_client::NestClient>>,
) -> Result<Vec<TrainedTopicRow>, TrainedTopicsError> {
    fauna_sync_engine::preference_surfaces::list_trained_topics(store, svc).await
}

/// Localize a service error.
///
/// A thin adapter over [`TrainedTopicsError::localized`], which owns the map —
/// this arm-for-arm shape is now shared with linux, whose own copy had drifted
/// into pasting the crate's `"config: "` / `"model: "` discriminant prefixes
/// onto user-facing text. The adapter stays so the call sites below keep
/// reading as a local function, beside its `localize_publish` siblings.
pub(super) fn localize(err: &TrainedTopicsError) -> String {
    err.localized()
}

/// The Trained topics facet's elements, appended into the Personalization
/// home's spine by [`super::labeler_catalog::personalization_elements`].
///
/// ui.yaml defines **no** empty-state id for this list (unlike the labelers
/// facet's `personalization-labelers-empty`), so none is invented here — an
/// empty facet paints its container, its input and its create button, and
/// nothing else.
pub(super) fn trained_topics_elements(state: &SettingsState) -> Vec<Element> {
    let tt = &state.trained_topics;
    // The single input serves both flows; the commit button's LABEL is what
    // tells the user which one it will run (linux's one-input-two-flows row
    // shape, which windows/apple/android all mirror).
    let commit_label = if tt.renaming.is_some() {
        p::TRAINED_FACTOR_SAVE
    } else {
        p::TRAINED_FACTOR_CREATE
    };
    let mut els = vec![
        // The facet's visible heading. ui.yaml gives this facet NO landmark id
        // of its own (the page's landmark is `personalization`), so it paints as
        // un-IDed chrome rather than inventing one — rule A.
        Element::chrome(p::TRAINED_TOPICS_TITLE),
        // The row container. Flat, like every other tui row list — the rows are
        // NOT registered under it, or its id would come first in every leaf's
        // path and break the scoped toggle read (see the module docs).
        Element::label(ids::PERSONALIZATION_TRAINED_FACTOR_LIST, String::new()),
        Element::input(
            ids::PERSONALIZATION_TRAINED_FACTOR_NAME_INPUT,
            tt.input.clone(),
            Field::Settings(SettingsField::TrainedFactorNameInput),
        )
        .labelled(p::TRAINED_FACTOR_PLACEHOLDER),
        Element::gesture_button(
            ids::PERSONALIZATION_TRAINED_FACTOR_CREATE_BUTTON,
            commit_label,
            !tt.busy,
            Gesture::Settings(Action::CommitTrainedFactorName),
        ),
    ];
    for (i, row) in tt.rows.iter().enumerate() {
        // The row landmark carries the composition key as a test-attr — the one
        // value no rendered text can spell (the name is the user's, the key is
        // the registry's), and the sealed registry means no wire read can
        // answer it either.
        //
        // ⚠ The attr is the **hex ALONE**, not the full `topic:<hex>` key: the
        // shared action prepends the prefix itself, because a web CSS class
        // cannot carry a `:` (`actions/personalization.py::topic_factor_key`).
        // Painting the whole key here yields `topic:topic:<hex>` downstream.
        //
        // `factor_key` is `None` only for a corrupt id, which has no
        // addressable model — such a row still renders (so the user can delete
        // it) but advertises no key.
        let mut item = Element::label(ids::PERSONALIZATION_TRAINED_FACTOR_ITEM, row.name.clone());
        if let Some(hex) = row
            .factor_key
            .as_deref()
            .and_then(|k| k.strip_prefix("topic:"))
        {
            item = item.attr("factor", hex);
        }
        els.push(item);
        els.push(Element::label(
            ids::PERSONALIZATION_TRAINED_FACTOR_NAME,
            row.name.clone(),
        ));
        els.push(Element::label(
            ids::PERSONALIZATION_TRAINED_FACTOR_EXAMPLE_COUNT,
            p::trained_factor_examples(&row.example_count.to_string()),
        ));
        // Scoped — the ONE leaf the action file reads with `scope=` (module
        // docs). `state` is spelled "true"/"false" here because that is what
        // `engagement_toggle_on` compares against; the feed's train verbs use
        // "on"/"off". Two vocabularies, two action files — read each one.
        els.push(
            Element::checkbox_gesture(
                ids::PERSONALIZATION_TRAINED_FACTOR_ENGAGEMENT_TOGGLE,
                p::TRAINED_FACTOR_ENGAGEMENT_TOGGLE,
                row.learn_from_engagement,
                Gesture::Settings(Action::ToggleTrainedFactorEngagement(i)),
            )
            .attr(
                "state",
                if row.learn_from_engagement {
                    "true"
                } else {
                    "false"
                },
            )
            .within(ids::PERSONALIZATION_TRAINED_FACTOR_ITEM, i),
        );
        els.push(Element::gesture_button(
            ids::PERSONALIZATION_TRAINED_FACTOR_RENAME_BUTTON,
            p::TRAINED_FACTOR_RENAME,
            !tt.busy,
            Gesture::Settings(Action::StartTrainedFactorRename(i)),
        ));
        els.push(Element::gesture_button(
            ids::PERSONALIZATION_TRAINED_FACTOR_DELETE_BUTTON,
            fauna_i18n::strings::common::DELETE,
            !tt.busy,
            Gesture::Settings(Action::DeleteTrainedFactor(i)),
        ));
        // "Publish…" — reveals the single-instance sheet against THIS row.
        // Flat, like its sibling row buttons (`open_publish_sheet` clicks it
        // with a plain `index=`).
        els.push(Element::gesture_button(
            ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_BUTTON,
            p::TRAINED_FACTOR_PUBLISH,
            !tt.busy,
            Gesture::Settings(Action::OpenTrainedFactorPublish(i)),
        ));
    }
    els.extend(publish_sheet_elements(&tt.publish));
    els
}

/// The review-prune sheet's elements — **empty while it is closed**, which is
/// how tui expresses "hidden": an unpainted element is not in the frame
/// registry, so `is_visible` answers false and `wait_for` blocks, exactly as a
/// GTK `set_visible(false)` reads to the same shared action.
///
/// **Row scoping.** The exemplar leaves are painted FLAT except the checkbox,
/// which `publish_exemplar_included` reads with a single-step
/// `scope="…-publish-exemplar-item[i]"` while `set_publish_exemplar_included`
/// *clicks* it flat — the trained-factor row's own mixed shape, satisfied the
/// same way: one checkbox per exemplar makes the flat occurrence index equal
/// the row index. The `-exemplar-list` container is painted flat and the items
/// are **not** registered under it, or its id would come first in the
/// checkbox's path and every scoped read would resolve to nothing while the
/// sheet painted perfectly (the `restore-history-item` finding).
fn publish_sheet_elements(sheet: &PublishSheetState) -> Vec<Element> {
    if !sheet.is_open() {
        return Vec::new();
    }
    let model = sheet.kind == PublishKind::Model;
    let mut els = vec![
        // The sheet container — what `open_publish_sheet` waits on.
        Element::label(
            ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_SHEET,
            p::PUBLISH_SHEET_TITLE,
        ),
        // The kind picker — a RAW-VALUE select whose options come from the
        // shared catalog, so the two option tokens are the same strings on all
        // 7 apps and the words the user reads cannot drift from the row they
        // produce (`fauna_core::format::publish_kind_options`).
        Element::select(
            ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_KIND_SELECT,
            sheet.kind.wire(),
            crate::element::SelectTarget::TrainedFactorPublishKind,
            fauna_core::format::publish_kind_options()
                .into_iter()
                .map(|o| o.value)
                .collect(),
        )
        .display_value(crate::format::publish_kind(sheet.kind.wire()))
        .labelled(p::PUBLISH_KIND_LABEL),
        // The mandated copy, PER KIND — § Publishing owns both sets and calls
        // the absence of any one line a bug, not a wording choice. The List
        // states its two accepted limitations (the corpus is only what this
        // device loaded; no author attribution); the Model states its three
        // (it generalizes; it discloses ≥3-post patterns including the dislike
        // half; anonymous and explicit-examples-only) plus the corpus-size line
        // that makes the rebuild's drops visible.
        Element::label(
            ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_LIMITATION_NOTE,
            if model {
                format!(
                    "{}\n{}",
                    p::PUBLISH_LIMITATION_NOTE_MODEL,
                    p::publish_corpus_size(
                        &sheet.included_examples.to_string(),
                        &sheet.marked_examples.to_string(),
                    )
                )
            } else {
                p::PUBLISH_LIMITATION_NOTE.to_string()
            },
        ),
        // Blank on every open — never seeded from the row's sealed name.
        Element::input(
            ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_NAME_INPUT,
            sheet.name.clone(),
            Field::Settings(SettingsField::TrainedFactorPublishNameInput),
        )
        .labelled(if model {
            p::PUBLISH_NAME_LABEL_MODEL
        } else {
            p::PUBLISH_NAME_LABEL
        }),
    ];
    if model {
        push_ngram_review(sheet, &mut els);
    } else {
        push_exemplar_review(sheet, &mut els);
    }
    // Publishing nothing is not a thing the artifact means, so the button says
    // so by going INSENSITIVE rather than by no-op'ing a click — a click that
    // does nothing and explains nothing is indistinguishable from a broken
    // sheet (linux's `restore-confirm-button` precedent). Covers both "the
    // corpus scored nothing" and "the user unchecked everything".
    els.push(Element::gesture_button(
        ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_SUBMIT_BUTTON,
        p::PUBLISH_SUBMIT,
        sheet.any_kept() && !sheet.busy,
        Gesture::Settings(Action::SubmitTrainedFactorPublish),
    ));
    els.push(Element::gesture_button(
        ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_CANCEL_BUTTON,
        fauna_i18n::strings::common::CANCEL,
        !sheet.busy,
        Gesture::Settings(Action::CancelTrainedFactorPublish),
    ));
    els
}

/// The List kind's review body — the scored top-N exemplars.
fn push_exemplar_review(sheet: &PublishSheetState, els: &mut Vec<Element>) {
    els.push(Element::chrome(p::PUBLISH_EXEMPLARS_TITLE));
    els.push(Element::label(
        ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_EXEMPLAR_LIST,
        String::new(),
    ));
    // Only once the read has RESOLVED — an in-flight corpus read is not an
    // empty corpus, and saying so would tell the user to go open a feed they
    // are already looking at.
    if sheet.scored && sheet.rows.is_empty() {
        els.push(Element::label(
            ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_EXEMPLAR_EMPTY,
            p::PUBLISH_EXEMPLARS_EMPTY,
        ));
    }
    for (i, (exemplar, keep)) in sheet.rows.iter().enumerate() {
        els.push(Element::label(
            ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_EXEMPLAR_ITEM,
            exemplar.preview.clone(),
        ));
        els.push(Element::label(
            ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_EXEMPLAR_TEXT,
            exemplar.preview.clone(),
        ));
        // The per-mille the artifact carries VERBATIM — the same number a
        // subscriber reads back at inspect, with no rescale anywhere between.
        els.push(Element::label(
            ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_EXEMPLAR_SCORE,
            p::publish_score(&exemplar.score.to_string()),
        ));
        // CHECKED = include (the `restore-kind-checkbox` prune shape): the user
        // edits the factor's own proposal rather than assembling one.
        els.push(
            Element::checkbox_gesture(
                ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_EXEMPLAR_CHECKBOX,
                p::PUBLISH_EXEMPLAR_INCLUDE,
                *keep,
                Gesture::Settings(Action::ToggleTrainedFactorPublishExemplar(i)),
            )
            .attr("state", if *keep { "true" } else { "false" })
            .within(ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_EXEMPLAR_ITEM, i),
        );
    }
}

/// The Model kind's review body — **every** surviving n-gram.
///
/// Where the List's exemplar rows bound *endorsement* of already-public ids,
/// these rows **are** the disclosure (`topic-factors.md` § Publishing), so
/// there is no top-N here: `TEXT_MODEL_PUBLISH_MAX_NGRAMS` is what keeps
/// "review the whole artifact" an honest promise, and rendering a capped
/// preview of it would break exactly that promise.
fn push_ngram_review(sheet: &PublishSheetState, els: &mut Vec<Element>) {
    els.push(Element::chrome(p::PUBLISH_NGRAMS_TITLE));
    els.push(Element::label(
        ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_NGRAM_LIST,
        String::new(),
    ));
    // The refusal state, and only once the scrub has RESOLVED — the exemplar
    // half's rule verbatim. An empty vocabulary means the 3-post floor admitted
    // nothing, which is a *refusal to publish*, not an empty list: the copy
    // says what would change it, because the floor is a privacy rule and the
    // only way through is genuinely more public examples.
    if sheet.scored && sheet.ngrams.is_empty() {
        els.push(Element::label(
            ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_NGRAM_EMPTY,
            p::PUBLISH_NGRAMS_EMPTY,
        ));
    }
    for (i, (ngram, keep)) in sheet.ngrams.iter().enumerate() {
        els.push(Element::label(
            ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_NGRAM_ITEM,
            ngram.ngram.clone(),
        ));
        els.push(Element::label(
            ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_NGRAM_TEXT,
            ngram.ngram.clone(),
        ));
        // Direction and count are BOTH shared faces, and both for the same
        // reason: the publisher's review is a promise about what a subscriber
        // reads back at `labeler-inspect-model-entry-*`, so the two ends must
        // not be able to disagree about what these counts mean.
        els.push(Element::label(
            ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_NGRAM_DIRECTION,
            crate::format::ngram_direction(ngram.more, ngram.less),
        ));
        els.push(Element::label(
            ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_NGRAM_COUNT,
            crate::format::ngram_doc_count(ngram.more, ngram.less),
        ));
        els.push(
            Element::checkbox_gesture(
                ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_NGRAM_CHECKBOX,
                p::PUBLISH_EXEMPLAR_INCLUDE,
                *keep,
                Gesture::Settings(Action::ToggleTrainedFactorPublishNgram(i)),
            )
            .attr("state", if *keep { "true" } else { "false" })
            .within(ids::PERSONALIZATION_TRAINED_FACTOR_PUBLISH_NGRAM_ITEM, i),
        );
    }
}

// ── Op construction (the only logic here; everything else is shared) ────────

/// The service handle. Takes only the `Send` `Arc<NestClient>` WS handle, so no
/// `&App` ever crosses a spawn boundary.
pub(super) fn service(
    nest: std::sync::Arc<fauna_client::NestClient>,
) -> TrainedTopics<std::sync::Arc<fauna_client::NestClient>> {
    TrainedTopics::new(nest)
}

/// The nav-edge list op, or `None` pre-login / with an undecodable secret (the
/// facet then renders no rows — never a panic, and never a silent partial:
/// every mutation reads through the same gate).
pub(super) fn load_op(state: &mut SettingsState) -> Option<super::Op> {
    let nest = state.nest.clone()?;
    // Pre-login gate: no identity, no load.
    state.identity_secret()?;
    state.trained_topics.dispatch_seq += 1;
    Some(super::Op::LoadTrainedTopics {
        nest,
        store: state.preference_store(),
        seq: state.trained_topics.dispatch_seq,
    })
}

/// A mutation op, arming `busy` as it goes.
///
/// `busy` is armed **only when an op is actually produced** — an ungated flip
/// would leave the facet permanently disabled pre-login (no op ⇒ no outcome ⇒
/// nothing to clear it), the "no error, no effect" shape the exhaustive
/// dispatch exists to retire (`muted_words_save_op`'s reasoning).
pub(super) fn gesture_op(
    state: &mut SettingsState,
    gesture: TrainedTopicsGesture,
) -> Option<super::Op> {
    let nest = state.nest.clone()?;
    // Pre-login gate: no identity, no gesture.
    state.identity_secret()?;
    let store = state.preference_store();
    state.trained_topics.busy = true;
    state.trained_topics.dispatch_seq += 1;
    Some(super::Op::TrainedTopicsGesture {
        nest,
        store,
        gesture,
        seq: state.trained_topics.dispatch_seq,
    })
}

/// `-create-button`: commit the name buffer — a rename when a row retargeted
/// it, a create otherwise. A blank buffer is refused locally (no round trip);
/// the shared service refuses it too, which is the authority — this is just not
/// spending a WS-RPC to be told so.
pub(super) fn commit_name_op(state: &mut SettingsState) -> Option<super::Op> {
    let name = state.trained_topics.input.trim().to_string();
    if name.is_empty() {
        return None;
    }
    let gesture = match state.trained_topics.renaming.clone() {
        Some(id) => TrainedTopicsGesture::Rename { id, name },
        None => TrainedTopicsGesture::Create(name),
    };
    gesture_op(state, gesture)
}

/// `-rename-button`: retarget the shared input at this row and seed it with the
/// current name, so the user edits rather than retypes.
pub(super) fn start_rename(state: &mut SettingsState, index: usize) {
    let Some(row) = state.trained_topics.row(index) else {
        return;
    };
    let (id, name) = (row.id.clone(), row.name.clone());
    state.trained_topics.renaming = Some(id);
    state.trained_topics.input = name;
}

/// `-delete-button`: the registry entry AND its paired sealed model row (the
/// shared service owns the ordering — registry first, then `model.delete`).
pub(super) fn delete_op(state: &mut SettingsState, index: usize) -> Option<super::Op> {
    let id = state.trained_topics.row(index)?.id.clone();
    // A delete of the row currently being renamed must not leave the input
    // retargeted at a factor that no longer exists — the next commit would then
    // rename nothing and look like a silent failure.
    if state.trained_topics.renaming.as_deref() == Some(id.as_slice()) {
        state.trained_topics.renaming = None;
        state.trained_topics.input.clear();
    }
    gesture_op(state, TrainedTopicsGesture::Delete(id))
}

/// `-engagement-toggle`: flip the row's Layer-A opt-in. Registry-only — what
/// engagement already taught the model is untouched (`topics.rs`).
pub(super) fn toggle_engagement_op(state: &mut SettingsState, index: usize) -> Option<super::Op> {
    let row = state.trained_topics.row(index)?;
    let (id, on) = (row.id.clone(), !row.learn_from_engagement);
    gesture_op(state, TrainedTopicsGesture::SetEngagement { id, on })
}

// ── The review-prune sheet (topic-factors.md § Publishing a trained factor) ──
//
// Both halves of the act are shared Rust and this module binds them, exactly as
// every other app's sheet does (priority #2):
// `FeedManager::score_corpus_for_factor` reads the corpus, and
// `publish::publish_trained_factor_list` owns the whole publish lifecycle.
// ⚠ Three things a per-app sheet must NOT re-implement, because the shared
// lifecycle already owns them and `topic-factors.md:106` says so outright: the
// `REVIEW_TOP_N` bound, the empty-entry-set refusal, and the signed `updated_at`
// stamp. They are the three knobs each shell could otherwise get differently
// wrong.

/// `-publish-button`: reveal the sheet against row `index` and score its
/// corpus.
///
/// Returns the corpus-read op, or `None` when the row has no addressable model
/// (a corrupt registry id has no `topic:<hex>` key, so there is nothing to
/// score) — in which case the sheet does not open at all rather than opening
/// onto a permanently empty review.
///
/// **The corpus is the LIVE manager's loaded window**, not a fresh one: a
/// second `FeedManager` would score an empty window and silently publish
/// nothing. That is why the caller passes the manager down from `App` instead
/// of this module building one.
pub(super) fn open_publish_op(
    state: &mut SettingsState,
    index: usize,
    manager: Option<std::sync::Arc<crate::feed::CliFeedManager>>,
) -> Option<super::Op> {
    let row = state.trained_topics.row(index)?;
    let factor_id = row.id.clone();
    let factor_key = row.factor_key.clone()?;
    // A re-open always starts from a clean sheet: a stale prune inherited from
    // a previous target would be a set of endorsements the user never made for
    // THIS factor.
    state.trained_topics.publish = PublishSheetState {
        target: Some(factor_id.clone()),
        ..PublishSheetState::default()
    };
    let manager = manager?;
    Some(super::Op::ScorePublishCorpus {
        manager,
        factor_id,
        factor_key,
    })
}

/// `-publish-kind-select`: swap which artifact kind the open sheet reviews.
///
/// **Re-reads from scratch.** The two kinds review different objects through
/// different shared faces, so the prune, the resolved bit, and the corpus-size
/// facts all belong to the kind that produced them: carrying `scored` across
/// would paint one kind's refusal state over the other's un-read corpus, and
/// carrying a prune would be a set of endorsements the user never made *for
/// this kind*. Picking the kind already selected is a no-op — a redundant
/// select must not discard a review in progress.
pub(super) fn set_publish_kind_op(
    state: &mut SettingsState,
    kind: PublishKind,
    manager: Option<std::sync::Arc<crate::feed::CliFeedManager>>,
) -> Option<super::Op> {
    let sheet = &mut state.trained_topics.publish;
    let factor_id = sheet.target.clone()?;
    if sheet.kind == kind {
        return None;
    }
    // The public name survives the swap: it is the user's own words about the
    // factor, and it is equally true of either kind — re-typing it would be
    // busywork, not safety.
    let name = std::mem::take(&mut sheet.name);
    *sheet = PublishSheetState {
        target: Some(factor_id.clone()),
        kind,
        name,
        ..PublishSheetState::default()
    };
    // The factor key is the manager's handle on the sealed model; resolve it
    // from the row rather than caching it on the sheet, so a row that vanished
    // under the open sheet cannot be re-read.
    let factor_key = state
        .trained_topics
        .rows
        .iter()
        .find(|r| r.id == factor_id)
        .and_then(|r| r.factor_key.clone())?;
    let manager = manager?;
    Some(match kind {
        PublishKind::List => super::Op::ScorePublishCorpus {
            manager,
            factor_id,
            factor_key,
        },
        PublishKind::Model => super::Op::ScrubPublishCorpus {
            manager,
            factor_id,
            factor_key,
        },
    })
}

/// Fold a **Model** corpus scrub into the open sheet.
///
/// Target-matched exactly as [`apply_scored_corpus`] is, and for the same
/// reason: a vocabulary scrubbed for factor A rendered under factor B would be
/// published under B's derived key and B's chosen name. The kind is checked
/// too — a scrub landing after the user switched back to List must not repaint
/// the List body's state.
pub(super) fn apply_scrubbed_corpus(
    state: &mut SettingsState,
    factor_id: &[u8],
    review: fauna_feed::TrainedModelReview,
) -> bool {
    let sheet = &mut state.trained_topics.publish;
    if sheet.target.as_deref() != Some(factor_id) || sheet.kind != PublishKind::Model {
        return false;
    }
    sheet.more_docs = review.more_docs;
    sheet.less_docs = review.less_docs;
    sheet.included_examples = review.included_examples;
    sheet.marked_examples = review.marked_examples;
    sheet.ngrams = review.ngrams.into_iter().map(|n| (n, true)).collect();
    sheet.scored = true;
    true
}

/// `-publish-ngram-checkbox[i]`: flip whether n-gram `i` survives the prune.
/// Out-of-range is a no-op, the facet's standing shape.
pub(super) fn toggle_publish_ngram(state: &mut SettingsState, index: usize) {
    if let Some((_, keep)) = state.trained_topics.publish.ngrams.get_mut(index) {
        *keep = !*keep;
    }
}

/// Fold a corpus read into the open sheet.
///
/// **Dropped unless it is the open target's** — the read is a nest round trip
/// (the sealed model), so a result scored for factor A must never render under
/// factor B: submit would then publish A's posts under B's derived key and B's
/// chosen name. linux guards this with a generation counter; the factor id
/// answers the same question directly here, and covers the close case too (no
/// target ⇒ nothing to render into).
pub(super) fn apply_scored_corpus(
    state: &mut SettingsState,
    factor_id: &[u8],
    exemplars: Vec<ScoredExemplar>,
) -> bool {
    if state.trained_topics.publish.target.as_deref() != Some(factor_id) {
        return false;
    }
    state.trained_topics.publish.rows = exemplars.into_iter().map(|e| (e, true)).collect();
    state.trained_topics.publish.scored = true;
    true
}

/// `-publish-exemplar-checkbox[i]`: flip whether exemplar `i` survives the
/// prune. Out-of-range is a no-op, the facet's standing shape.
pub(super) fn toggle_publish_exemplar(state: &mut SettingsState, index: usize) {
    if let Some((_, keep)) = state.trained_topics.publish.rows.get_mut(index) {
        *keep = !*keep;
    }
}

/// `-publish-cancel-button`: close without publishing, dropping the target and
/// the reviewed rows so a re-open cannot inherit a stale prune.
pub(super) fn cancel_publish(state: &mut SettingsState) {
    state.trained_topics.publish = PublishSheetState::default();
}

/// `-publish-submit-button`: publish what survived the prune.
///
/// A blank name is refused locally (no round trip), the sibling create/rename
/// flow's guard — the shared lifecycle refuses it too, which is the authority;
/// this is just not spending a WS-RPC to be told so.
pub(super) fn submit_publish_op(state: &mut SettingsState) -> Option<super::Op> {
    let name = state.trained_topics.publish.name.trim().to_string();
    if name.is_empty() {
        return None;
    }
    let target = state.trained_topics.publish.target.clone()?;
    // A non-16-byte registry id has no addressable model, so it cannot have been
    // scored either — unreachable from a rendered row, refused here so the
    // invariant holds at the call and not only at the widget.
    let factor_id = <[u8; 16]>::try_from(target.as_slice()).ok()?;
    let sheet = &state.trained_topics.publish;
    let op = match sheet.kind {
        PublishKind::List => {
            let entries = sheet.kept();
            if entries.is_empty() {
                return None;
            }
            let nest = state.nest.clone()?;
            let secret = state.identity_secret()?;
            super::Op::PublishTrainedFactorList {
                nest,
                secret,
                factor_id,
                name,
                entries,
            }
        }
        PublishKind::Model => {
            let ngrams = sheet.kept_ngrams();
            if ngrams.is_empty() {
                return None;
            }
            // ⚠ `more_docs`/`less_docs` go over UNSHRUNK by the prune — they say
            // how many public examples the vocabulary was built from, and they
            // are the posterior's priors and the damp's sample count, so
            // matching them to a pruned vocabulary would make the published
            // model look more confident than it is (the shared lifecycle's
            // documented rule; the shell must not "helpfully" recount).
            let (more_docs, less_docs) = (sheet.more_docs, sheet.less_docs);
            let nest = state.nest.clone()?;
            let secret = state.identity_secret()?;
            super::Op::PublishTrainedFactorModel {
                nest,
                secret,
                factor_id,
                name,
                more_docs,
                less_docs,
                ngrams,
            }
        }
    };
    state.trained_topics.publish.busy = true;
    Some(op)
}

/// Localize the shared crate's typed publish errors. Every variant is already a
/// complete sentence naming the offending row where it has one, so — as with
/// [`localize`] — only a variant needing a *number the crate cannot know* would
/// be special-cased, and none does (linux/apple/android/windows all landed on
/// the same passthrough).
pub(super) fn localize_publish(err: &PublishListError) -> String {
    err.to_string()
}

/// The Model lifecycle's error twin. Separate enum, same passthrough rule and
/// the same reason: every variant is already a complete sentence, so the shell
/// has nothing a `to_string` does not already say.
pub(super) fn localize_publish_model(err: &PublishModelError) -> String {
    err.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::SubPage;

    fn row(name: &str, examples: u32, engagement: bool) -> TrainedTopicRow {
        TrainedTopicRow {
            id: vec![7u8; 16],
            name: name.to_string(),
            factor_key: Some(format!("topic:{}", "ab".repeat(16))),
            example_count: examples,
            learn_from_engagement: engagement,
        }
    }

    fn state_with(rows: Vec<TrainedTopicRow>) -> SettingsState {
        let mut state = SettingsState {
            sub: SubPage::Personalization,
            ..Default::default()
        };
        state.trained_topics.rows = rows;
        state
    }

    fn ids(els: &[Element]) -> Vec<String> {
        els.iter().map(|e| e.id.clone()).collect()
    }

    /// The facet's static ids paint with no data at all. ui.yaml defines no
    /// empty-state id for this list, so an empty facet paints exactly these
    /// four and no row leaves.
    #[test]
    fn the_empty_facet_paints_its_static_ids_and_no_rows() {
        let els = trained_topics_elements(&state_with(vec![]));
        let ids = ids(&els);
        for id in [
            "personalization-trained-factor-list",
            "personalization-trained-factor-name-input",
            "personalization-trained-factor-create-button",
        ] {
            assert!(
                ids.contains(&id.to_string()),
                "missing {id:?}; have {ids:?}"
            );
        }
        assert!(
            !ids.contains(&"personalization-trained-factor-item".to_string()),
            "an empty facet must paint no rows; have {ids:?}"
        );
    }

    /// One row per factor, with the name and the advisory example count.
    #[test]
    fn a_populated_facet_paints_one_row_per_factor() {
        let els = trained_topics_elements(&state_with(vec![
            row("Cats", 3, false),
            row("Boats", 0, false),
        ]));
        let ids = ids(&els);
        assert_eq!(
            ids.iter()
                .filter(|id| *id == "personalization-trained-factor-item")
                .count(),
            2,
            "one row per factor; have {ids:?}"
        );
        let names: Vec<&str> = els
            .iter()
            .filter(|e| e.id == "personalization-trained-factor-name")
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(names, vec!["Cats", "Boats"], "names render in list order");
        let counts: Vec<&str> = els
            .iter()
            .filter(|e| e.id == "personalization-trained-factor-example-count")
            .map(|e| e.text.as_str())
            .collect();
        assert!(
            counts[0].contains('3'),
            "the advisory example count renders; got {:?}",
            counts[0]
        );
    }

    /// The load-bearing scoping contract: the engagement toggle's path
    /// **starts** with `personalization-trained-factor-item[i]`, which is what
    /// makes `engagement_toggle_on`'s single-step scoped read resolve — while
    /// the flat leaves stay flat, because their action reads pass a plain
    /// `index=`. Getting either half wrong paints a perfect page that the
    /// e2e cannot read.
    #[test]
    fn the_engagement_toggle_is_scoped_under_its_row_and_the_rest_are_flat() {
        let els = trained_topics_elements(&state_with(vec![
            row("Cats", 1, true),
            row("Boats", 0, false),
        ]));
        let toggles: Vec<&Element> = els
            .iter()
            .filter(|e| e.id == "personalization-trained-factor-engagement-toggle")
            .collect();
        assert_eq!(toggles.len(), 2);
        for (i, t) in toggles.iter().enumerate() {
            assert_eq!(
                t.path.first().map(|(c, idx)| (c.as_str(), *idx)),
                Some(("personalization-trained-factor-item", i)),
                "toggle {i} must be scoped under its own row; path = {:?}",
                t.path
            );
        }
        for flat in [
            "personalization-trained-factor-name",
            "personalization-trained-factor-example-count",
            "personalization-trained-factor-rename-button",
            "personalization-trained-factor-delete-button",
        ] {
            assert!(
                els.iter()
                    .filter(|e| e.id == flat)
                    .all(|e| e.path.is_empty()),
                "{flat} is read with a plain index= and must paint FLAT"
            );
        }
    }

    /// The toggle's `state` attr is what the action file compares against, and
    /// it is spelled "true"/"false" here (the feed's train verbs use
    /// "on"/"off"). tui emits no implicit attrs, so an omission reads as a
    /// permanently-off toggle.
    #[test]
    fn the_engagement_toggle_publishes_its_state_attr() {
        let els = trained_topics_elements(&state_with(vec![
            row("Cats", 1, true),
            row("Boats", 0, false),
        ]));
        let states: Vec<&str> = els
            .iter()
            .filter(|e| e.id == "personalization-trained-factor-engagement-toggle")
            .map(|e| {
                e.attrs
                    .iter()
                    .find(|(k, _)| k == "state")
                    .map(|(_, v)| v.as_str())
                    .unwrap_or("<missing>")
            })
            .collect();
        assert_eq!(states, vec!["true", "false"]);
    }

    /// The row landmark carries the composition key the create-feed picker
    /// selects by — the one value no rendered text spells.
    #[test]
    fn the_row_advertises_its_factor_key_as_an_attr() {
        let els = trained_topics_elements(&state_with(vec![row("Cats", 0, false)]));
        let item = els
            .iter()
            .find(|e| e.id == "personalization-trained-factor-item")
            .expect("a row paints");
        let key = item
            .attrs
            .iter()
            .find(|(k, _)| k == "factor")
            .map(|(_, v)| v.as_str());
        assert_eq!(
            key,
            Some("ab".repeat(16)).as_deref(),
            "the attr carries the HEX ALONE — the shared action prepends \
             `topic:` itself, so painting the full key double-prefixes it"
        );
    }

    /// The single name input serves both flows, and the commit button's label
    /// is what says which one it will run.
    #[test]
    fn the_commit_button_relabels_while_a_rename_is_retargeted() {
        let mut state = state_with(vec![row("Cats", 0, false)]);
        let create = trained_topics_elements(&state);
        let create_label = create
            .iter()
            .find(|e| e.id == "personalization-trained-factor-create-button")
            .map(|e| e.text.clone())
            .unwrap();
        start_rename(&mut state, 0);
        assert_eq!(
            state.trained_topics.input, "Cats",
            "a rename seeds the input with the current name so the user edits"
        );
        let rename = trained_topics_elements(&state);
        let rename_label = rename
            .iter()
            .find(|e| e.id == "personalization-trained-factor-create-button")
            .map(|e| e.text.clone())
            .unwrap();
        assert_ne!(create_label, rename_label);
    }

    /// Deleting the row a rename is retargeted at must clear the retarget —
    /// otherwise the next commit renames a factor that no longer exists and
    /// reads to the user as a silent failure.
    #[test]
    fn deleting_the_renamed_row_clears_the_retarget() {
        let mut state = state_with(vec![row("Cats", 0, false)]);
        start_rename(&mut state, 0);
        assert!(state.trained_topics.renaming.is_some());
        // No nest ⇒ no op, but the local retarget bookkeeping still runs.
        let _ = delete_op(&mut state, 0);
        assert!(
            state.trained_topics.renaming.is_none() && state.trained_topics.input.is_empty(),
            "the retarget must not survive its own row's delete"
        );
    }

    /// A gesture against a row index that no longer exists is a no-op, not a
    /// panic — the shape that keeps a click landing after a concurrent delete
    /// (or an e2e racing a re-render) harmless.
    #[test]
    fn an_out_of_range_row_gesture_is_a_no_op() {
        let mut state = state_with(vec![]);
        assert!(delete_op(&mut state, 3).is_none());
        assert!(toggle_engagement_op(&mut state, 3).is_none());
        start_rename(&mut state, 3);
        assert!(state.trained_topics.renaming.is_none());
    }

    /// A blank buffer never spends a round trip.
    #[test]
    fn a_blank_name_produces_no_op() {
        let mut state = state_with(vec![]);
        state.trained_topics.input = "   ".to_string();
        assert!(commit_name_op(&mut state).is_none());
        assert!(
            !state.trained_topics.busy,
            "a refused commit must not arm busy — nothing would ever clear it"
        );
    }

    /// The cap message names the limit, which is the whole reason the shared
    /// boundary hands back a code rather than a sentence.
    #[test]
    fn the_cap_error_names_the_limit() {
        let msg = localize(&TrainedTopicsError::Cap(32));
        assert!(
            msg.contains("32"),
            "the cap message must name the limit: {msg}"
        );
    }

    // ── The review-prune sheet (topic-factors.md § Publishing) ──────────────

    fn exemplar(post_id: &str, preview: &str, score: i64) -> ScoredExemplar {
        ScoredExemplar {
            post_id: post_id.to_string(),
            preview: preview.to_string(),
            score,
        }
    }

    /// A state with the sheet already open against the one row, scored with
    /// `exemplars` — the shape every assertion below starts from.
    fn state_with_open_sheet(exemplars: Vec<ScoredExemplar>) -> SettingsState {
        let mut state = state_with(vec![row("Cats", 2, false)]);
        // Reaches through the real gestures rather than hand-building the
        // sheet, so the tests pin the code path the click takes. No manager ⇒
        // no op, but the local open bookkeeping still runs.
        let _ = open_publish_op(&mut state, 0, None);
        let id = state.trained_topics.publish.target.clone().unwrap();
        apply_scored_corpus(&mut state, &id, exemplars);
        state
    }

    /// A closed sheet paints NOTHING — that absence is how tui says "hidden",
    /// and an unpainted element is not in the frame registry, so `is_visible`
    /// answers false exactly as a GTK `set_visible(false)` does.
    #[test]
    fn a_closed_sheet_paints_none_of_its_ids() {
        let els = trained_topics_elements(&state_with(vec![row("Cats", 0, false)]));
        let ids = ids(&els);
        for id in ids.iter() {
            assert!(
                !id.starts_with("personalization-trained-factor-publish-sheet")
                    && !id.contains("-publish-exemplar")
                    && !id.contains("-publish-submit")
                    && !id.contains("-publish-cancel"),
                "a closed sheet must paint no sheet ids; found {id:?}"
            );
        }
        // …but every row still offers the button that opens it.
        assert_eq!(
            ids.iter()
                .filter(|id| *id == "personalization-trained-factor-publish-button")
                .count(),
            1,
            "each row offers Publish…; have {ids:?}"
        );
    }

    /// One publish button per factor row, so `open_publish_sheet(index)`'s flat
    /// `index=` addresses the row the user clicked.
    #[test]
    fn every_row_gets_its_own_publish_button_painted_flat() {
        let els = trained_topics_elements(&state_with(vec![
            row("Cats", 1, false),
            row("Boats", 0, false),
        ]));
        let buttons: Vec<&Element> = els
            .iter()
            .filter(|e| e.id == "personalization-trained-factor-publish-button")
            .collect();
        assert_eq!(buttons.len(), 2);
        assert!(
            buttons.iter().all(|b| b.path.is_empty()),
            "the publish button is clicked with a plain index= and must paint FLAT"
        );
    }

    /// An open sheet paints every static id ui.yaml lists for it.
    #[test]
    fn an_open_sheet_paints_its_static_ids() {
        let state = state_with_open_sheet(vec![exemplar("aa", "cats are great", 812)]);
        let ids = ids(&trained_topics_elements(&state));
        for id in [
            "personalization-trained-factor-publish-sheet",
            "personalization-trained-factor-publish-limitation-note",
            "personalization-trained-factor-publish-name-input",
            "personalization-trained-factor-publish-exemplar-list",
            "personalization-trained-factor-publish-submit-button",
            "personalization-trained-factor-publish-cancel-button",
        ] {
            assert!(
                ids.contains(&id.to_string()),
                "missing {id:?}; have {ids:?}"
            );
        }
    }

    /// **The privacy invariant of the two-names split.** The public name input
    /// starts blank on every open and is NEVER seeded from the row's sealed
    /// registry name — prefilling would leak the user's own private label into
    /// a public artifact by default (`topic-factors.md:102`).
    #[test]
    fn the_public_name_input_starts_blank_and_never_inherits_the_sealed_name() {
        let mut state = state_with(vec![row("Cats", 2, false)]);
        // Even with the sibling create/rename buffer holding the sealed name…
        state.trained_topics.input = "Cats".to_string();
        let _ = open_publish_op(&mut state, 0, None);
        assert_eq!(
            state.trained_topics.publish.name, "",
            "the PUBLIC name must start blank — the sealed name is private"
        );
        // …and a second open after the user typed a public name starts blank
        // again rather than re-offering the previous list's name.
        state.trained_topics.publish.name = "Small orange cats".to_string();
        let _ = open_publish_op(&mut state, 0, None);
        assert_eq!(state.trained_topics.publish.name, "");
    }

    /// Exemplars arrive CHECKED: the user edits the factor's own proposal
    /// rather than assembling one (the `restore-kind-checkbox` prune shape).
    #[test]
    fn exemplars_default_to_included() {
        let state = state_with_open_sheet(vec![
            exemplar("aa", "cats", 900),
            exemplar("bb", "budget spreadsheet", 120),
        ]);
        assert!(
            state.trained_topics.publish.rows.iter().all(|(_, k)| *k),
            "every exemplar must default to included — the user PRUNES"
        );
        let els = trained_topics_elements(&state);
        let states: Vec<&str> = els
            .iter()
            .filter(|e| e.id == "personalization-trained-factor-publish-exemplar-checkbox")
            .map(|e| {
                e.attrs
                    .iter()
                    .find(|(k, _)| k == "state")
                    .map(|(_, v)| v.as_str())
                    .unwrap_or("<missing>")
            })
            .collect();
        assert_eq!(
            states,
            vec!["true", "true"],
            "the `state` attr is what `publish_exemplar_included` reads"
        );
    }

    /// The load-bearing scoping contract, the sibling row's shape: the include
    /// checkbox's path **starts** with `…-publish-exemplar-item[i]` (its scoped
    /// read), while every other leaf stays flat (their plain `index=` reads).
    /// Getting either half wrong paints a perfect sheet the e2e cannot read.
    #[test]
    fn the_exemplar_checkbox_is_scoped_under_its_row_and_the_rest_are_flat() {
        let state = state_with_open_sheet(vec![
            exemplar("aa", "cats", 900),
            exemplar("bb", "boats", 120),
        ]);
        let els = trained_topics_elements(&state);
        let boxes: Vec<&Element> = els
            .iter()
            .filter(|e| e.id == "personalization-trained-factor-publish-exemplar-checkbox")
            .collect();
        assert_eq!(boxes.len(), 2);
        for (i, b) in boxes.iter().enumerate() {
            assert_eq!(
                b.path.first().map(|(c, idx)| (c.as_str(), *idx)),
                Some(("personalization-trained-factor-publish-exemplar-item", i)),
                "checkbox {i} must be scoped under its own exemplar row; path = {:?}",
                b.path
            );
        }
        for flat in [
            "personalization-trained-factor-publish-exemplar-item",
            "personalization-trained-factor-publish-exemplar-text",
            "personalization-trained-factor-publish-exemplar-score",
        ] {
            assert!(
                els.iter()
                    .filter(|e| e.id == flat)
                    .all(|e| e.path.is_empty()),
                "{flat} is read with a plain index= and must paint FLAT"
            );
        }
    }

    /// Best-scoring first (the shared corpus read's order, preserved verbatim),
    /// and the per-mille renders — the same number the artifact carries and a
    /// subscriber reads back at inspect, with no rescale in between.
    #[test]
    fn exemplars_render_in_corpus_order_with_their_per_mille_scores() {
        let state = state_with_open_sheet(vec![
            exemplar("aa", "cats are great", 812),
            exemplar("bb", "budget spreadsheet", 40),
        ]);
        let els = trained_topics_elements(&state);
        let texts: Vec<&str> = els
            .iter()
            .filter(|e| e.id == "personalization-trained-factor-publish-exemplar-text")
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(texts, vec!["cats are great", "budget spreadsheet"]);
        let scores: Vec<&str> = els
            .iter()
            .filter(|e| e.id == "personalization-trained-factor-publish-exemplar-score")
            .map(|e| e.text.as_str())
            .collect();
        assert!(
            scores[0].contains("812") && scores[1].contains("40"),
            "the per-mille renders verbatim; got {scores:?}"
        );
    }

    /// Submit is INSENSITIVE with nothing kept and re-arms once something is —
    /// publishing an empty List means nothing, and a click that does nothing
    /// and explains nothing is indistinguishable from a broken sheet.
    #[test]
    fn submit_disarms_with_nothing_kept_and_rearms_when_something_survives() {
        let mut state = state_with_open_sheet(vec![
            exemplar("aa", "cats", 900),
            exemplar("bb", "boats", 120),
        ]);
        let submit_enabled = |s: &SettingsState| {
            trained_topics_elements(s)
                .iter()
                .find(|e| e.id == "personalization-trained-factor-publish-submit-button")
                .map(|e| e.enabled)
                .expect("submit paints")
        };
        assert!(submit_enabled(&state), "armed with both kept");
        toggle_publish_exemplar(&mut state, 0);
        assert!(submit_enabled(&state), "still armed with one kept");
        toggle_publish_exemplar(&mut state, 1);
        assert!(
            !submit_enabled(&state),
            "unchecking everything must DISARM submit, not no-op the click"
        );
        toggle_publish_exemplar(&mut state, 1);
        assert!(
            submit_enabled(&state),
            "re-arms once something is kept again"
        );
    }

    /// Only what survived the prune is handed to the shared publish call, with
    /// each entry's own per-mille — the promise the whole feature makes is that
    /// subscribers see exactly what the publisher chose to show.
    #[test]
    fn only_the_kept_exemplars_reach_the_publish_call() {
        let mut state = state_with_open_sheet(vec![
            exemplar("aa", "cats", 900),
            exemplar("bb", "boats", 120),
            exemplar("cc", "budget", 30),
        ]);
        toggle_publish_exemplar(&mut state, 1);
        assert_eq!(
            state.trained_topics.publish.kept(),
            vec![("aa".to_string(), 900), ("cc".to_string(), 30)],
            "the pruned row must not ride along, and scores travel verbatim"
        );
    }

    /// "Not scored yet" and "scored nothing" are different pictures, and only
    /// the second may say so: painting the empty state while the corpus read is
    /// still in flight would tell the user to go open a feed they are already
    /// looking at.
    #[test]
    fn the_empty_state_waits_for_the_read_to_resolve() {
        let mut state = state_with(vec![row("Cats", 0, false)]);
        let _ = open_publish_op(&mut state, 0, None);
        let empty_painted = |s: &SettingsState| {
            ids(&trained_topics_elements(s))
                .contains(&"personalization-trained-factor-publish-exemplar-empty".to_string())
        };
        assert!(
            !empty_painted(&state),
            "an in-flight corpus read is not an empty corpus"
        );
        let id = state.trained_topics.publish.target.clone().unwrap();
        apply_scored_corpus(&mut state, &id, vec![]);
        assert!(
            empty_painted(&state),
            "a resolved read that scored nothing says so"
        );
    }

    /// **The retarget guard.** A corpus read is a nest round trip, so a result
    /// scored for factor A must never render under factor B — submit would then
    /// publish A's posts under B's derived key and B's chosen name.
    #[test]
    fn a_corpus_read_for_another_factor_is_dropped() {
        let mut state = state_with_open_sheet(vec![exemplar("aa", "cats", 900)]);
        let landed = apply_scored_corpus(
            &mut state,
            &[0xEEu8; 16],
            vec![exemplar("zz", "someone else's posts", 999)],
        );
        assert!(!landed, "a mismatched factor's exemplars must be refused");
        assert_eq!(
            state.trained_topics.publish.rows.len(),
            1,
            "the open target's own review must be untouched"
        );
        assert_eq!(state.trained_topics.publish.rows[0].0.post_id, "aa");
    }

    /// A read landing after the user cancelled has nowhere to go — there is no
    /// target, so there is nothing it could be a review OF.
    #[test]
    fn a_corpus_read_landing_after_cancel_is_dropped() {
        let mut state = state_with(vec![row("Cats", 0, false)]);
        let _ = open_publish_op(&mut state, 0, None);
        let id = state.trained_topics.publish.target.clone().unwrap();
        cancel_publish(&mut state);
        assert!(!apply_scored_corpus(
            &mut state,
            &id,
            vec![exemplar("aa", "cats", 900)]
        ));
        assert!(
            state.trained_topics.publish.rows.is_empty() && !state.trained_topics.publish.is_open(),
            "a cancelled sheet must not be re-populated from behind"
        );
    }

    /// Re-opening starts from a clean sheet: a prune inherited from a previous
    /// target would be a set of public endorsements the user never made for
    /// THIS factor.
    #[test]
    fn reopening_resets_the_prune() {
        let mut state = state_with_open_sheet(vec![
            exemplar("aa", "cats", 900),
            exemplar("bb", "boats", 120),
        ]);
        toggle_publish_exemplar(&mut state, 0);
        let _ = open_publish_op(&mut state, 0, None);
        assert!(
            state.trained_topics.publish.rows.is_empty() && !state.trained_topics.publish.scored,
            "a re-open must not inherit the previous review"
        );
    }

    /// A blank public name never spends a round trip — the sibling
    /// create/rename guard, and the shared lifecycle refuses it too.
    #[test]
    fn a_blank_public_name_produces_no_publish_op() {
        let mut state = state_with_open_sheet(vec![exemplar("aa", "cats", 900)]);
        state.trained_topics.publish.name = "   ".to_string();
        assert!(submit_publish_op(&mut state).is_none());
        assert!(
            !state.trained_topics.publish.busy,
            "a refused submit must not arm busy — nothing would ever clear it"
        );
    }

    /// Nothing kept ⇒ no op even if the disarmed button were somehow activated:
    /// the "never publish an empty list" invariant holds at the CALL, not only
    /// at the widget.
    #[test]
    fn an_empty_prune_produces_no_publish_op() {
        let mut state = state_with_open_sheet(vec![exemplar("aa", "cats", 900)]);
        state.trained_topics.publish.name = "Small orange cats".to_string();
        toggle_publish_exemplar(&mut state, 0);
        assert!(submit_publish_op(&mut state).is_none());
        assert!(!state.trained_topics.publish.busy);
    }

    /// Navigating away closes the sheet — a half-finished prune is not a draft
    /// worth restoring, it is public endorsements re-shown out of context.
    #[test]
    fn a_fresh_visit_closes_the_sheet() {
        let mut state = state_with_open_sheet(vec![exemplar("aa", "cats", 900)]);
        assert!(state.trained_topics.publish.is_open());
        state.trained_topics.reset_form();
        assert!(!state.trained_topics.publish.is_open());
        assert!(state.trained_topics.publish.rows.is_empty());
    }

    /// A row whose registry id is corrupt has no addressable model, so there is
    /// nothing to score — the sheet must not open onto a permanently empty
    /// review that looks like a broken page.
    #[test]
    fn a_row_with_no_factor_key_never_opens_the_sheet() {
        let mut state = state_with(vec![TrainedTopicRow {
            factor_key: None,
            ..row("Corrupt", 0, false)
        }]);
        assert!(open_publish_op(&mut state, 0, None).is_none());
        assert!(
            !state.trained_topics.publish.is_open(),
            "no key ⇒ no corpus read ⇒ the sheet must stay closed"
        );
    }

    /// An out-of-range gesture is a no-op, not a panic — the facet's standing
    /// shape, extended to the sheet's own leaves.
    #[test]
    fn out_of_range_publish_gestures_are_no_ops() {
        let mut state = state_with(vec![]);
        assert!(open_publish_op(&mut state, 3, None).is_none());
        toggle_publish_exemplar(&mut state, 3);
        assert!(state.trained_topics.publish.rows.is_empty());
    }

    /// The typed publish errors reach the user as their own sentences — the
    /// passthrough every other app landed on.
    #[test]
    fn a_publish_refusal_localizes_to_the_crates_own_sentence() {
        let msg = localize_publish(&PublishListError::NoEntries);
        assert!(!msg.is_empty(), "a refusal must say something");
        assert_eq!(msg, PublishListError::NoEntries.to_string());
        let model = localize_publish_model(&PublishModelError::EmptyVocabulary);
        assert!(
            !model.is_empty(),
            "the Model refusal must say something too"
        );
        assert_eq!(model, PublishModelError::EmptyVocabulary.to_string());
    }

    // ── The Model kind (topic-factors.md § Publishing, v2) ──────────────────

    fn ngram(text: &str, more: u32, less: u32) -> fauna_feed::ReviewNgram {
        fauna_feed::ReviewNgram {
            ngram: text.to_string(),
            more,
            less,
        }
    }

    /// A state with the sheet open on the **Model** kind and scrubbed with
    /// `ngrams` — the Model twin of [`state_with_open_sheet`], reaching through
    /// the real gestures for the same reason.
    fn state_with_open_model_sheet(
        ngrams: Vec<fauna_feed::ReviewNgram>,
        included: u32,
        marked: u32,
    ) -> SettingsState {
        let mut state = state_with(vec![row("Cats", 2, false)]);
        let _ = open_publish_op(&mut state, 0, None);
        let _ = set_publish_kind_op(&mut state, PublishKind::Model, None);
        let id = state.trained_topics.publish.target.clone().unwrap();
        apply_scrubbed_corpus(
            &mut state,
            &id,
            fauna_feed::TrainedModelReview {
                more_docs: 4,
                less_docs: 2,
                included_examples: included,
                marked_examples: marked,
                ngrams,
            },
        );
        state
    }

    /// The picker paints on both kinds, round-trips the WIRE discriminator, and
    /// offers exactly the shared catalog's two options — the cross-app contract
    /// a driver names, which no rewording of the copy may break.
    #[test]
    fn the_kind_select_round_trips_the_wire_discriminator_from_the_shared_catalog() {
        let list = state_with_open_sheet(vec![exemplar("aa", "a cat", 900)]);
        let el = trained_topics_elements(&list)
            .into_iter()
            .find(|e| e.id == "personalization-trained-factor-publish-kind-select")
            .expect("the kind select paints on the List kind too");
        assert_eq!(el.text, "list", "the default kind is the weaker disclosure");
        match &el.role {
            crate::element::Role::Select { options, .. } => assert_eq!(
                options,
                &vec!["list".to_string(), "text-model".to_string()],
                "options come from fauna_core::format::publish_kind_options"
            ),
            other => panic!("the kind picker must be a Select, got {other:?}"),
        }

        let model = state_with_open_model_sheet(vec![ngram("orange cat", 3, 0)], 3, 5);
        let el = trained_topics_elements(&model)
            .into_iter()
            .find(|e| e.id == "personalization-trained-factor-publish-kind-select")
            .expect("kind select");
        assert_eq!(el.text, "text-model");
    }

    /// The two kinds review different objects, so the bodies are mutually
    /// exclusive: painting an exemplar row under the Model kind would offer a
    /// prune over a set that submit does not read.
    #[test]
    fn the_kind_swaps_the_whole_review_body() {
        let model = state_with_open_model_sheet(vec![ngram("orange cat", 3, 0)], 3, 5);
        let painted = ids(&trained_topics_elements(&model));
        assert!(
            painted
                .iter()
                .any(|i| i == "personalization-trained-factor-publish-ngram-item"),
            "the Model body must paint its n-gram rows"
        );
        assert!(
            !painted.iter().any(|i| i.contains("-publish-exemplar")),
            "the Model body must paint NO exemplar ids; got {painted:?}"
        );

        let list = state_with_open_sheet(vec![exemplar("aa", "a cat", 900)]);
        let painted = ids(&trained_topics_elements(&list));
        assert!(
            !painted.iter().any(|i| i.contains("-publish-ngram")),
            "the List body must paint NO n-gram ids; got {painted:?}"
        );
    }

    /// EVERY survivor renders — the vocabulary IS the disclosure, so there is
    /// no top-N here, and each row carries the three facts § Publishing
    /// ratifies (text, class direction, distinct-doc count) plus its prune box.
    #[test]
    fn every_surviving_ngram_renders_with_its_direction_and_class_blind_count() {
        // The middle row is a TIE and the last is dislike-dominant: both are
        // states the direction column exists to make visible, and both would be
        // silently mis-stated by a `more > less` boolean.
        let state = state_with_open_model_sheet(
            vec![
                ngram("orange cat", 4, 0),
                ngram("small dog", 2, 2),
                ngram("tax advice", 1, 3),
            ],
            5,
            9,
        );
        let els = trained_topics_elements(&state);
        let texts: Vec<&str> = els
            .iter()
            .filter(|e| e.id == "personalization-trained-factor-publish-ngram-text")
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(
            texts,
            vec!["orange cat", "small dog", "tax advice"],
            "all three survivors render, in the scrub's informativeness order"
        );

        let directions: Vec<&str> = els
            .iter()
            .filter(|e| e.id == "personalization-trained-factor-publish-ngram-direction")
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(directions.len(), 3);
        assert_ne!(
            directions[0], directions[2],
            "a like-dominant and a dislike-dominant row must not read the same"
        );
        assert_ne!(
            directions[1], directions[0],
            "a tie is its own answer, not rounded to the dominant side"
        );

        let counts: Vec<&str> = els
            .iter()
            .filter(|e| e.id == "personalization-trained-factor-publish-ngram-count")
            .map(|e| e.text.as_str())
            .collect();
        // Class-blind: 1 + 3 = 4, never the dominant class's 3 — the sum is the
        // quantity the 3-post privacy floor bounds.
        assert!(
            counts[2].contains('4'),
            "the count must be class-blind (more + less); got {:?}",
            counts[2]
        );
    }

    /// The prune checkbox is scoped under its own row (the exemplar half's
    /// contract) and defaults to INCLUDED — the user edits the scrub's proposal
    /// rather than assembling one.
    #[test]
    fn ngram_checkboxes_default_to_included_and_scope_under_their_row() {
        let state = state_with_open_model_sheet(
            vec![ngram("orange cat", 4, 0), ngram("tax advice", 1, 3)],
            5,
            5,
        );
        let els = trained_topics_elements(&state);
        let boxes: Vec<&Element> = els
            .iter()
            .filter(|e| e.id == "personalization-trained-factor-publish-ngram-checkbox")
            .collect();
        assert_eq!(boxes.len(), 2);
        for (i, b) in boxes.iter().enumerate() {
            assert_eq!(
                b.attrs
                    .iter()
                    .find(|(k, _)| k == "state")
                    .map(|(_, v)| v.as_str()),
                Some("true"),
                "n-gram {i} must start included"
            );
            assert_eq!(
                b.path.first().map(|(c, idx)| (c.as_str(), *idx)),
                Some(("personalization-trained-factor-publish-ngram-item", i)),
                "n-gram {i}'s checkbox must be scoped under its own row"
            );
        }
    }

    /// The refusal state waits for the scrub to RESOLVE — an in-flight read is
    /// not an empty vocabulary, and saying so would send the user off to mark
    /// posts they have already marked.
    #[test]
    fn the_ngram_refusal_state_waits_for_the_scrub_to_resolve() {
        let mut state = state_with(vec![row("Cats", 2, false)]);
        let _ = open_publish_op(&mut state, 0, None);
        let _ = set_publish_kind_op(&mut state, PublishKind::Model, None);
        assert!(
            !ids(&trained_topics_elements(&state))
                .iter()
                .any(|i| i == "personalization-trained-factor-publish-ngram-empty"),
            "an unresolved scrub must not paint the refusal"
        );

        let id = state.trained_topics.publish.target.clone().unwrap();
        apply_scrubbed_corpus(&mut state, &id, fauna_feed::TrainedModelReview::default());
        assert!(
            ids(&trained_topics_elements(&state))
                .iter()
                .any(|i| i == "personalization-trained-factor-publish-ngram-empty"),
            "a resolved-and-empty scrub IS the refusal state"
        );
    }

    /// The corpus-size line rides the mandated note and shows BOTH numbers —
    /// `M - N` is what the rebuild dropped as private, deleted, or unreachable,
    /// and a publisher only learns that happened if both are visible.
    #[test]
    fn the_model_note_states_the_mandated_copy_and_both_corpus_numbers() {
        let state = state_with_open_model_sheet(vec![ngram("orange cat", 4, 0)], 3, 7);
        let note = trained_topics_elements(&state)
            .into_iter()
            .find(|e| e.id == "personalization-trained-factor-publish-limitation-note")
            .expect("limitation note");
        assert!(
            note.text.contains('3') && note.text.contains('7'),
            "the note must state N of M: {:?}",
            note.text
        );
        assert_ne!(
            note.text,
            p::PUBLISH_LIMITATION_NOTE,
            "the Model kind must NOT reuse the List's copy — § Publishing owns two sets"
        );
    }

    /// Submit sends only what survived the prune — and the class doc counters
    /// go over UNSHRUNK, because they say how many public examples the
    /// vocabulary was built from, not how many rows the user kept.
    #[test]
    fn only_kept_ngrams_publish_and_the_doc_counters_do_not_shrink() {
        let mut state = state_with_open_model_sheet(
            vec![
                ngram("orange cat", 4, 0),
                ngram("tax advice", 1, 3),
                ngram("small dog", 2, 1),
            ],
            5,
            5,
        );
        toggle_publish_ngram(&mut state, 1);

        let sheet = &state.trained_topics.publish;
        assert_eq!(
            sheet
                .kept_ngrams()
                .iter()
                .map(|(n, _, _)| n.clone())
                .collect::<Vec<_>>(),
            vec!["orange cat".to_string(), "small dog".to_string()],
            "the unchecked n-gram must not cross"
        );
        assert_eq!(
            sheet.kept_ngrams(),
            vec![
                ("orange cat".to_string(), 4, 0),
                ("small dog".to_string(), 2, 1),
            ],
            "each survivor's own per-class counts travel verbatim"
        );
        // 4 and 2 are the SCRUB's counters, and pruning must not touch them.
        // Recounting them from the kept rows (2 more-docs' worth here) is
        // exactly the over-confidence the shared lifecycle forbids: they are
        // the posterior's priors and the damp's sample count.
        assert_eq!(
            (sheet.more_docs, sheet.less_docs),
            (4, 2),
            "the class doc counters must not shrink with the prune"
        );
    }

    /// Swapping the kind discards the prune and re-reads — carrying `scored`
    /// across would paint one kind's refusal state over the other's un-read
    /// corpus. The public name survives: it is the user's own words about the
    /// factor and is equally true of either kind.
    #[test]
    fn swapping_the_kind_discards_the_prune_but_keeps_the_public_name() {
        let mut state = state_with_open_sheet(vec![exemplar("aa", "a cat", 900)]);
        state.trained_topics.publish.name = "Orange cats".to_string();
        toggle_publish_exemplar(&mut state, 0);

        let _ = set_publish_kind_op(&mut state, PublishKind::Model, None);
        let sheet = &state.trained_topics.publish;
        assert_eq!(sheet.kind, PublishKind::Model);
        assert!(sheet.rows.is_empty(), "the List prune must not survive");
        assert!(!sheet.scored, "the Model corpus has not been read yet");
        assert_eq!(sheet.name, "Orange cats", "the public name survives");
        assert!(sheet.target.is_some(), "the sheet stays open on its factor");
    }

    /// Re-picking the kind already selected is a NO-OP — a redundant select
    /// (an e2e re-driving the picker, a user clicking the current value) must
    /// not throw away a review the user has already pruned.
    #[test]
    fn re_selecting_the_open_kind_does_not_discard_the_review() {
        let mut state = state_with_open_model_sheet(vec![ngram("orange cat", 4, 0)], 3, 3);
        toggle_publish_ngram(&mut state, 0);
        assert!(set_publish_kind_op(&mut state, PublishKind::Model, None).is_none());
        assert_eq!(
            state.trained_topics.publish.ngrams.len(),
            1,
            "the vocabulary survives a redundant select"
        );
        assert!(
            !state.trained_topics.publish.ngrams[0].1,
            "and so does the prune"
        );
    }

    /// A scrub that lands after the user switched back to List is DROPPED — the
    /// target guard's kind half. Rendering it would put a vocabulary under a
    /// body that publishes exemplars, and submit reads the body.
    #[test]
    fn a_scrub_landing_after_a_switch_back_to_list_is_dropped() {
        let mut state = state_with_open_model_sheet(vec![], 0, 0);
        let id = state.trained_topics.publish.target.clone().unwrap();
        let _ = set_publish_kind_op(&mut state, PublishKind::List, None);

        assert!(
            !apply_scrubbed_corpus(
                &mut state,
                &id,
                fauna_feed::TrainedModelReview {
                    ngrams: vec![ngram("orange cat", 4, 0)],
                    ..Default::default()
                }
            ),
            "a scrub for a kind no longer open must be refused"
        );
        assert!(state.trained_topics.publish.ngrams.is_empty());
    }

    /// A vocabulary scrubbed for one factor must never render — or publish —
    /// under another: submit would sign it with the other factor's derived key
    /// and the other factor's chosen name.
    #[test]
    fn a_scrub_for_another_factor_is_dropped() {
        let mut state = state_with_open_model_sheet(vec![], 0, 0);
        assert!(
            !apply_scrubbed_corpus(
                &mut state,
                b"some-other-fact",
                fauna_feed::TrainedModelReview {
                    ngrams: vec![ngram("orange cat", 4, 0)],
                    ..Default::default()
                }
            ),
            "a scrub for another factor must be refused"
        );
        assert!(state.trained_topics.publish.ngrams.is_empty());
    }

    /// Submit is disarmed with nothing kept — per KIND. An untouched List prune
    /// left over in state must not arm a Model submit that would send nothing.
    #[test]
    fn model_submit_disarms_with_nothing_kept() {
        let mut state = state_with_open_model_sheet(vec![ngram("orange cat", 4, 0)], 3, 3);
        state.trained_topics.publish.name = "Orange cats".to_string();
        // Seed a *List* prune that is fully kept: only the Model body may arm
        // the Model submit.
        state.trained_topics.publish.rows = vec![(exemplar("aa", "a cat", 900), true)];
        toggle_publish_ngram(&mut state, 0);

        let submit = trained_topics_elements(&state)
            .into_iter()
            .find(|e| e.id == "personalization-trained-factor-publish-submit-button")
            .expect("submit button");
        assert!(
            !submit.enabled,
            "an empty Model prune must disarm submit even with a full List prune in state"
        );
        assert!(
            !state.trained_topics.publish.any_kept(),
            "and the arming predicate itself must read the Model body"
        );
    }

    /// Out-of-range is a no-op on the Model leaves too.
    #[test]
    fn out_of_range_ngram_gestures_are_no_ops() {
        let mut state = state_with(vec![]);
        toggle_publish_ngram(&mut state, 3);
        assert!(state.trained_topics.publish.ngrams.is_empty());
    }
}
