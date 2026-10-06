//! The Settings → Personalization home + Settings → Community labelers catalog
//! (`docs/goal/architecture/content-moderation-and-ranking.md` § Tier-3 community
//! models & background re-processing; the `personalization` and `labeler-catalog`
//! pages of `tests/e2e-unified/ui.yaml`). Both rail slots sit directly after Muted
//! words, as its sibling personalization surfaces — `docs/goal/ui/settings.md`
//! § Navigation model.
//!
//! **Two pages, ONE machine.** A community labeler is a tier-3 model someone else
//! published; the catalog page browses every published one and the personalization
//! home shows only the ones this user subscribes to. Both render the SAME
//! `LabelerCatalogSnapshot` (`libs/fauna-labeler-catalog-machine`) — the home is
//! that snapshot's `entries` client-filtered to `subscribed == true`, never a
//! second fetch. linux is the reference leg
//! (`apps/fauna-linux/src/views/personalization/mod.rs`); tui is the seventh and
//! last app to lift it (priority #1).
//!
//! **Inspect-before-subscribe is the point, not decoration.** Subscribing grants a
//! labeler the right to read and label the user's content, so the frame requires
//! the full signed metadata be shown *before* the grant — and the machine
//! re-verifies the hash/size/signature binding client-side rather than trusting
//! the nest's word for it (`LabelerInspectView::verified`). This module paints
//! that view; it never decides anything about it.
//!
//! **A dumb renderer over the shared machine.** Every decision belongs to
//! `LabelerCatalogMachine`: the wire→view transcription, the artifact-kind
//! normalization (absent/empty ⇒ `wasm`), the refresh-after-mutate sequencing, and
//! the `grant_id: None` v1 subscribe posture. Direct Rust, no FFI hop — tui is the
//! machine's second direct consumer after linux.
//!
//! **The gesture index is the SNAPSHOT index, not the painted row.** The machine's
//! `inspect`/`subscribe`/`unsubscribe` address rows by their position in the full,
//! unfiltered `entries` vec, while the personalization home paints a *filtered*
//! list — so painting row 0 of the home and dispatching index 0 would unsubscribe
//! whichever labeler happens to sit first in the catalog. Each row therefore
//! carries its own snapshot index in the `Action` it was built with, exactly as
//! linux's `rebuild_rows` pairs `(index, entry)`. Pinned by
//! [`tests::the_home_dispatches_the_snapshot_index_not_the_painted_row`].
//!
//! **The error bridge is load-bearing** — the `mail_lists.rs` reasoning: a gesture
//! failure arrives on the *snapshot* (`LabelerCatalogSnapshot.error`), never on the
//! call's return value (every machine gesture returns `()`), so the fold copies it
//! onto `App::errors`. Without it a rejected subscribe would paint no error and
//! have no effect — the dropped-command shape testing.md point 11 forbids.

use fauna_ui_ids as ids;
use std::sync::Arc;

use fauna_i18n::strings::{labeler_catalog as t, personalization as p};
use fauna_labeler_catalog_machine::{
    LabelerCatalogEntry, LabelerCatalogMachine, LabelerCatalogSnapshot, LabelerInspectView,
};

use super::{Action, SettingsState};
use crate::element::{Element, Gesture};
use crate::pages::Page;

/// The state both pages render from — the shared machine plus its last snapshot.
///
/// The machine is built once at the post-auth hook (`attach_session`), the
/// `MailListsState` shape: construction is sync and cheap (the RPC is `refresh()`),
/// it carries interior mutability, and holding it as an `Arc` is what lets an `Op`
/// own only `Arc`s and cross a `tokio::spawn`. Session-scoped — `clear_session`
/// drops it, so one account's subscriptions never outlive a switch.
#[derive(Default)]
pub struct LabelerCatalogState {
    /// The shared machine. `None` pre-login.
    pub machine: Option<Arc<LabelerCatalogMachine>>,
    /// The last snapshot either page painted. `None` until the nav-edge refresh
    /// folds one — until then both pages are still LOADING, which is not the
    /// same picture as empty (see [`LabelerCatalogState::loaded`]).
    pub snapshot: Option<LabelerCatalogSnapshot>,
}

/// tui's observer: deliberately a no-op.
///
/// The machine's observer exists for the *reactive* apps — linux re-renders its
/// widget tree on `on_changed`. ratatui is immediate-mode: there is no persistent
/// tree to invalidate, and every op's own fold re-reads the snapshot before the
/// next frame paints (the agent awaits the op before its next read — `tui.md`
/// § Architecture's one-awaited-op-per-gesture contract). A notifying observer
/// would therefore be a *second*, racing path to the same paint, not an extra
/// safety net.
///
/// Not the crate's `NullObserver` — that one is `cfg(test, feature =
/// "test-observer")`, so reaching for it would compile a test-only surface into a
/// shipped binary.
struct ImmediateModeObserver;

impl fauna_labeler_catalog_machine::LabelerCatalogObserver for ImmediateModeObserver {
    fn on_changed(&self) {}
}

impl LabelerCatalogState {
    /// Build the shared machine from the session's WS handle **with its grant
    /// seams**: subscribing a `wasm` mail labeler mints the per-labeler grant
    /// to this nest's mail service and unsubscribing revokes it
    /// (`content-moderation-and-ranking.md` § Tier-3 → *Subscribing = minting
    /// a capability*), which needs the actor's identity key — to sign the
    /// grant-log events and seal the grant ledger the log lives in — exactly as
    /// the Nests page's machine does (`mail_glue::build_linked_nests_machine_with_trust`).
    /// tui is the lead app; the six lift through the same shared builder.
    ///
    /// A `secret_hex` that will not decode (the launch flow validated it to
    /// reach Online, so this is an unrecoverable identity fault) falls back to
    /// the grant-less machine with a warning, so the pages still browse,
    /// inspect and (un)subscribe rather than vanish — the Nests page's own
    /// fallback (`settings/nests.rs`).
    pub(super) fn build(
        nest: Arc<fauna_client::NestClient>,
        secret_hex: &str,
        ledger: Arc<dyn fauna_client_config::SuccessionLedgerStore>,
        mail: Arc<dyn fauna_client_config::MailStore>,
    ) -> Self {
        let observer = Arc::new(ImmediateModeObserver);
        let machine = match fauna_core::identity::ActorKeypair::from_secret_hex(secret_hex) {
            Ok(keypair) => {
                fauna_labeler_catalog_machine::nest_api::build_labeler_catalog_machine_with_grants(
                    nest, keypair, ledger, mail, observer,
                )
            }
            Err(e) => {
                tracing::warn!(
                    "labeler catalog: decode secret_hex: {e}; building without grant seams — \
                     a mail labeler subscription will not mint its grant"
                );
                fauna_labeler_catalog_machine::nest_api::build_labeler_catalog_machine(
                    nest, observer,
                )
            }
        };
        Self {
            machine: Some(machine),
            snapshot: None,
        }
    }

    /// The snapshot's entries, or an empty slice before the first refresh.
    pub(crate) fn entries(&self) -> &[LabelerCatalogEntry] {
        self.snapshot
            .as_ref()
            .map(|s| s.entries.as_slice())
            .unwrap_or(&[])
    }

    /// Whether the catalog read has RESOLVED — the second painting condition of
    /// both empty-state elements (`docs/goal/ui/README.md` § *List pages:
    /// loading is not empty*; the field's rationale lives on
    /// `LabelerCatalogSnapshot::loaded`).
    ///
    /// Two ways to be unloaded, and both must suppress the empty state: no
    /// snapshot folded yet (pre-login, or before the nav-edge refresh returns),
    /// and a folded snapshot whose only read FAILED.
    fn loaded(&self) -> bool {
        self.snapshot.as_ref().is_some_and(|s| s.loaded)
    }

    /// The open inspect view, if the panel is showing.
    pub(crate) fn inspecting(&self) -> Option<&LabelerInspectView> {
        self.snapshot.as_ref()?.inspecting.as_ref()
    }

    /// Each subscribed labeler's `labeler:<hex>` composition key, in snapshot
    /// order — the create-feed picker's other dynamic source beside the trained
    /// factors (`content-moderation-and-ranking.md` § Composition: subscribing
    /// makes a labeler's labels a ranking factor the user can weight).
    ///
    /// Only the SUBSCRIBED rows: composing a labeler the user has not
    /// subscribed to would write a composition entry the nest has no scores
    /// for, so the picker would be offering an option that silently does
    /// nothing. Empty before the first refresh resolves, which is why the
    /// catalog is loaded post-auth rather than only on a Settings visit.
    pub fn subscribed_factors(&self) -> Vec<String> {
        self.subscribed()
            .into_iter()
            .map(|(_, e)| e.factor.clone())
            .collect()
    }

    /// The subscribed rows paired with their SNAPSHOT index — the personalization
    /// home's render source. The pairing is what keeps a home gesture addressing
    /// the labeler the user actually clicked (see the module docs).
    fn subscribed(&self) -> Vec<(u32, &LabelerCatalogEntry)> {
        self.entries()
            .iter()
            .enumerate()
            .filter(|(_, e)| e.subscribed)
            .map(|(i, e)| (i as u32, e))
            .collect()
    }
}

/// Which async machine gesture an [`super::Op::LabelerCatalogGesture`] carries.
///
/// One op for all three because the shared machine's contract is identical in
/// each case — await the gesture, then re-read the snapshot (which the machine has
/// already refreshed on success and stamped with `error` on failure). The `u32` is
/// always the SNAPSHOT index; see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelerGesture {
    Inspect(u32),
    Subscribe(u32),
    Unsubscribe(u32),
}

impl LabelerGesture {
    /// Run this gesture against the machine. Every arm returns `()` — a failure
    /// lands on the snapshot's `error`, which is why the fold's error bridge is
    /// the only thing standing between a rejected subscribe and a silent no-op.
    pub(super) async fn run(self, machine: &LabelerCatalogMachine) {
        match self {
            LabelerGesture::Inspect(i) => machine.inspect(i).await,
            LabelerGesture::Subscribe(i) => machine.subscribe(i).await,
            LabelerGesture::Unsubscribe(i) => machine.unsubscribe(i).await,
        }
    }
}

/// The Personalization home's ordered element list — the page spine plus the
/// SUBSCRIBED-labelers facet.
///
/// **Scope note.** This page's other two facets (the trained topic factors of
/// `behavior/topic-factors.md`, and the engagement-cue controls of
/// `behavior/engagement-cues.md`) are separate feature families, unbuilt on tui and
/// tracked as their own slices — see `ui-actual-tui.yaml`'s `personalization`
/// block. Their absence is why this list is shorter than ui.yaml's `elements`.
///
/// The page's `error-message` is registered globally by
/// [`crate::ui::register_frame`] (the privacy/logs/muted-words precedent).
pub(super) fn personalization_elements(state: &SettingsState) -> Vec<Element> {
    let lc = &state.labeler_catalog;
    let subscribed = lc.subscribed();
    let mut els = vec![
        Element::label(ids::PAGE_HEADING, p::TITLE),
        // The page landmark the driver waits on.
        Element::label("personalization", p::TITLE),
        // Leaves Settings entirely for the Feed page — the facet is "your feeds",
        // which live on the feed page's create-feed surface, not here.
        Element::gesture_button(
            ids::PERSONALIZATION_FEEDS_LINK,
            p::FEEDS_LINK,
            true,
            Gesture::Nav(Page::Feed),
        )
        .nav(),
        // Stays inside this settings shell and swaps to the already-shipped
        // muted-words sub-page (reused, never rebuilt here).
        Element::gesture_button(
            ids::PERSONALIZATION_MUTED_WORDS_LINK,
            p::MUTED_WORDS_LINK,
            true,
            Gesture::Settings(Action::OpenMutedWords),
        )
        .nav(),
        // The row container. Flat, like every other tui row list — see
        // [`labeler_catalog_elements`] for why.
        Element::label(ids::PERSONALIZATION_LABELERS_LIST, String::new()),
    ];
    // Only once the catalog read has RESOLVED — an in-flight read is not an
    // empty subscription list, and saying so would tell the user they subscribe
    // to nothing while the rows are on their way (the `-publish-exemplar-empty`
    // gate's reasoning, `trained_topics.rs`, and the same three-state rule the
    // Media page's `media-empty-state` follows).
    if lc.loaded() && subscribed.is_empty() {
        els.push(Element::label(
            ids::PERSONALIZATION_LABELERS_EMPTY,
            p::LABELERS_EMPTY,
        ));
    }
    for (row, (index, entry)) in subscribed.iter().enumerate() {
        // `show_inspect_subscribe: false` — the home is the "what I already
        // granted" facet; browsing and granting happen on the catalog page.
        push_labeler_row(&mut els, entry, *index, row, false);
    }
    els.push(Element::gesture_button(
        ids::PERSONALIZATION_BROWSE_CATALOG_BUTTON,
        p::BROWSE_CATALOG,
        true,
        Gesture::Settings(Action::OpenLabelerCatalog),
    ));
    // The page's second facet — the user's trainable tier-1 topic factors
    // (`behavior/topic-factors.md`). A sibling module over a different shared
    // seam, appended here because ui.yaml makes both facets one page.
    els.extend(super::trained_topics::trained_topics_elements(state));
    // The page's THIRD facet — the engagement-cue controls
    // (`behavior/engagement-cues.md` §§ Layer A / Layer B). Another sibling
    // module over another shared seam, appended for the same reason.
    els.extend(super::engagement_cues::engagement_cue_elements(
        &state.engagement,
    ));
    els.push(
        Element::gesture_button(
            ids::SETTINGS_NAV_BACK,
            fauna_i18n::strings::common::BACK,
            true,
            Gesture::Settings(Action::NavBack),
        )
        .nav_back(),
    );
    els
}

/// The Community-labelers catalog's ordered element list — every published
/// labeler, plus the inspect-before-subscribe panel when it is open.
///
/// **Row scoping.** The shared action reads every row leaf with a single-step
/// `scope="labeler-catalog-item[i]"` (`actions/labeler_catalog.py`), and the
/// registry prefix-matches paths — so each leaf's path must START with
/// `labeler-catalog-item`. Hence `.within(ids::LABELER_CATALOG_ITEM, row)` on the
/// leaves and a FLAT `labeler-catalog-list` / `labeler-catalog-item`: nesting the
/// rows under the list container would put that container first in every leaf's
/// path and leave each scoped read resolving to nothing while the page painted
/// perfectly.
pub(super) fn labeler_catalog_elements(state: &SettingsState) -> Vec<Element> {
    let lc = &state.labeler_catalog;
    let entries = lc.entries();
    let mut els = vec![
        Element::label(ids::PAGE_HEADING, t::TITLE),
        // The page landmark the driver waits on.
        Element::label(ids::LABELER_CATALOG, t::TITLE),
        // Flat — see the doc comment.
        Element::label(ids::LABELER_CATALOG_LIST, String::new()),
    ];
    // Same gate as the home facet above: "nobody has published a labeler" is a
    // claim only a read that RETURNED can make.
    if lc.loaded() && entries.is_empty() {
        els.push(Element::label(ids::LABELER_CATALOG_EMPTY, t::EMPTY));
    }
    for (row, entry) in entries.iter().enumerate() {
        // On the catalog the painted row and the snapshot index coincide; the
        // pair is passed explicitly anyway so the two never silently fuse.
        push_labeler_row(&mut els, entry, row as u32, row, true);
    }
    if let Some(view) = lc.inspecting() {
        push_inspect_panel(
            &mut els,
            view,
            Gesture::Settings(Action::LabelerCloseInspect),
        );
    }
    els.push(
        Element::gesture_button(
            ids::SETTINGS_NAV_BACK,
            fauna_i18n::strings::common::BACK,
            true,
            Gesture::Settings(Action::NavBack),
        )
        .nav_back(),
    );
    els
}

/// One `labeler-catalog-item` row, shared by both pages.
///
/// `index` addresses the machine (the snapshot position); `row` is where this row
/// paints (the scope the driver reads). They differ on the filtered home — see the
/// module docs.
///
/// `show_inspect_subscribe` is the one behavioural difference between the pages:
/// the catalog offers inspect + subscribe, the home neither (it lists what the
/// user already granted). **Unsubscribe renders on both** whenever the row is
/// subscribed, and exactly one of subscribe/unsubscribe ever paints per row — the
/// gating the shared action's `wait_for_subscribed_state` polls on.
fn push_labeler_row(
    els: &mut Vec<Element>,
    entry: &LabelerCatalogEntry,
    index: u32,
    row: usize,
    show_inspect_subscribe: bool,
) {
    els.push(Element::label(ids::LABELER_CATALOG_ITEM, String::new()));
    // The kind badge is the ONE field that is not a verbatim passthrough, and
    // only in one case: a `text-model` whose tokenizer contract this build does
    // not implement says so here, because the compose seam leaves that factor
    // inert and this row is where the user learns why
    // (`content-moderation-and-ranking.md` § Tier-3 artifact kinds — the "and
    // says so" half). The predicate and the wording are both shared
    // (`fauna_core::format::text_model_needs_newer_app`, over
    // `scoring::text_model_version_supported` — the same function the seam's
    // inert branch reads), so a badge that disagreed with the scorer is not
    // expressible here. `None` = paint the kind verbatim, which is every
    // ordinary row.
    let kind_text = match fauna_core::format::text_model_needs_newer_app(
        &entry.artifact_kind,
        entry.artifact_version,
    ) {
        Some(note) => crate::wizard::localized(&note),
        None => entry.artifact_kind.clone(),
    };
    // Every other field renders the snapshot's value VERBATIM — the machine
    // already normalized `artifact_kind`, and re-deriving a default here is
    // exactly what its doc comment forbids.
    for (id, text) in [
        (
            "labeler-catalog-item-publisher",
            entry.publisher_actor.clone(),
        ),
        ("labeler-catalog-item-kind", kind_text),
        (
            "labeler-catalog-item-content-kind",
            entry.content_kind.clone(),
        ),
        ("labeler-catalog-item-version", entry.version.to_string()),
        ("labeler-catalog-item-factor", entry.factor.clone()),
    ] {
        els.push(Element::label(id, text).within(ids::LABELER_CATALOG_ITEM, row));
    }
    if show_inspect_subscribe {
        els.push(
            Element::gesture_button(
                ids::LABELER_CATALOG_ITEM_INSPECT_BUTTON,
                t::INSPECT,
                true,
                Gesture::Settings(Action::LabelerInspect(index)),
            )
            .within(ids::LABELER_CATALOG_ITEM, row),
        );
        if !entry.subscribed {
            els.push(
                Element::gesture_button(
                    ids::LABELER_CATALOG_ITEM_SUBSCRIBE_BUTTON,
                    t::SUBSCRIBE,
                    true,
                    Gesture::Settings(Action::LabelerSubscribe(index)),
                )
                .within(ids::LABELER_CATALOG_ITEM, row),
            );
        }
    }
    if entry.subscribed {
        els.push(
            Element::gesture_button(
                ids::LABELER_CATALOG_ITEM_UNSUBSCRIBE_BUTTON,
                t::UNSUBSCRIBE,
                true,
                Gesture::Settings(Action::LabelerUnsubscribe(index)),
            )
            .within(ids::LABELER_CATALOG_ITEM, row),
        );
    }
}

/// The inspect-before-subscribe panel — the trust gate.
///
/// The metadata block mirrors linux's field set and order verbatim (priority #3:
/// same concepts everywhere), including `verified`, which is the machine's own
/// re-verification of the hash/size/signature binding rather than the nest's
/// claim. The raw artifact bytes are deliberately NOT rendered: the panel shows
/// facts *about* the module, never its source.
///
/// Shared with the room settings editor, which paints this same view in place
/// for `room-labeler-inspect-button[i]` (`ui/conversations.md` § Element IDs);
/// `close` is the gesture each host wires to `labeler-inspect-close-button`.
pub(crate) fn push_inspect_panel(
    els: &mut Vec<Element>,
    view: &LabelerInspectView,
    close: Gesture,
) {
    els.push(Element::label(ids::LABELER_INSPECT_PANEL, String::new()));
    els.push(Element::label(
        ids::LABELER_INSPECT_METADATA,
        format!(
            "labeler_id: {}\nversion: {}\nartifact_kind: {}\nwasm_hash: {}\nwasm_size: {}\n\
             needs_text: {}\nneeds_hashtags: {}\nneeds_media_metadata: {}\nneeds_author: {}\n\
             needs_attachment_bytes: {}\nverified: {}",
            view.labeler_id,
            view.version,
            view.artifact_kind,
            view.wasm_hash,
            view.wasm_size,
            view.needs_text,
            view.needs_hashtags,
            view.needs_media_metadata,
            view.needs_author,
            view.needs_attachment_bytes,
            view.verified,
        ),
    ));
    // The list-kind section is `optional_elements` in ui.yaml — it registers only
    // for a `list` artifact, so a `wasm` inspect leaves these absent rather than
    // painting empty rows a driver could read as real.
    if view.artifact_kind == "list" {
        els.push(Element::label(
            ids::LABELER_INSPECT_LIST_NAME,
            match &view.list_name {
                // An unnamed list is valid (pre-name artifacts) — say so rather
                // than rendering a blank line.
                Some(name) => t::list_name(name),
                None => t::UNNAMED_LIST.to_string(),
            },
        ));
        els.push(Element::label(
            ids::LABELER_INSPECT_LIST_ENTRY_COUNT,
            t::list_entry_count(&view.list_entries.len().to_string()),
        ));
        // Flat container + flat rows, the `labeler-catalog-item` shape.
        els.push(Element::label(
            ids::LABELER_INSPECT_LIST_ENTRIES,
            String::new(),
        ));
        for (i, entry) in view.list_entries.iter().enumerate() {
            els.push(Element::label(
                ids::LABELER_INSPECT_LIST_ENTRY,
                String::new(),
            ));
            els.push(
                Element::label(ids::LABELER_INSPECT_LIST_ENTRY_ID, entry.content_id.clone())
                    .within(ids::LABELER_INSPECT_LIST_ENTRY, i),
            );
            // The publisher's per-mille score verbatim — no rescale anywhere in
            // the chain, so what the publish sheet showed is what inspect shows.
            els.push(
                Element::label(
                    ids::LABELER_INSPECT_LIST_ENTRY_SCORE,
                    entry.score.to_string(),
                )
                .within(ids::LABELER_INSPECT_LIST_ENTRY, i),
            );
        }
    }
    // The text-model section, the same `optional_elements` shape: it registers
    // only for a `text-model` artifact, keyed on the KIND rather than on the
    // payload being non-empty — an empty vocabulary is a decode failure, not a
    // reason to render the panel as if it were some other kind.
    if view.artifact_kind == fauna_core::scoring::artifact_kind::TEXT_MODEL {
        els.push(Element::label(
            ids::LABELER_INSPECT_MODEL_NAME,
            match &view.model_name {
                Some(name) => t::model_name(name),
                None => t::UNNAMED_MODEL.to_string(),
            },
        ));
        els.push(Element::label(
            ids::LABELER_INSPECT_MODEL_NGRAM_COUNT,
            t::model_ngram_count(&view.model_ngrams.len().to_string()),
        ));
        els.push(Element::label(
            ids::LABELER_INSPECT_MODEL_ENTRIES,
            String::new(),
        ));
        // EVERY entry — the vocabulary is the model's whole matching surface,
        // and a capped preview is not the artifact. `TEXT_MODEL_PUBLISH_MAX_NGRAMS`
        // is what keeps that promise renderable.
        for (i, entry) in view.model_ngrams.iter().enumerate() {
            els.push(Element::label(
                ids::LABELER_INSPECT_MODEL_ENTRY,
                String::new(),
            ));
            els.push(
                Element::label(ids::LABELER_INSPECT_MODEL_ENTRY_TEXT, entry.ngram.clone())
                    .within(ids::LABELER_INSPECT_MODEL_ENTRY, i),
            );
            // The SAME two shared faces the publisher's review rows painted, so
            // what a publisher was shown before publishing is what a subscriber
            // reads before subscribing.
            els.push(
                Element::label(
                    ids::LABELER_INSPECT_MODEL_ENTRY_DIRECTION,
                    crate::format::ngram_direction(entry.more, entry.less),
                )
                .within(ids::LABELER_INSPECT_MODEL_ENTRY, i),
            );
            els.push(
                Element::label(
                    ids::LABELER_INSPECT_MODEL_ENTRY_COUNT,
                    crate::format::ngram_doc_count(entry.more, entry.less),
                )
                .within(ids::LABELER_INSPECT_MODEL_ENTRY, i),
            );
        }
    }
    els.push(Element::gesture_button(
        ids::LABELER_INSPECT_CLOSE_BUTTON,
        t::CLOSE_INSPECT,
        true,
        close,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::Role;
    use crate::settings::SubPage;
    use fauna_labeler_catalog_machine::{LabelerInspectListEntry, LabelerInspectModelNgram};

    fn entry(tag: &str, subscribed: bool) -> LabelerCatalogEntry {
        LabelerCatalogEntry {
            labeler_id: tag.repeat(32),
            version: 1,
            publisher_actor: format!("pub-{tag}"),
            artifact_kind: "wasm".to_string(),
            content_kind: "post".to_string(),
            factor: format!("labeler:{}", tag.repeat(32)),
            wasm_hash: "ab".repeat(36),
            wasm_size: 1024,
            subscribed,
            ..Default::default()
        }
    }

    /// A snapshot whose entries deliberately do NOT start with a subscribed row —
    /// so a filtered-index bug cannot pass by coincidence.
    ///
    /// `loaded: true` — this is the post-read picture every render test wants;
    /// the unloaded one is exercised on its own by
    /// [`tests::neither_page_paints_its_empty_state_before_the_read_resolves`].
    /// A catalog row for a `text-model` at `version`, so the badge's two arms
    /// can be driven from the same fixture.
    fn model_entry(tag: &str, artifact_version: u64) -> LabelerCatalogEntry {
        LabelerCatalogEntry {
            artifact_kind: "text-model".to_string(),
            artifact_version,
            ..entry(tag, false)
        }
    }

    fn state_with(entries: Vec<LabelerCatalogEntry>) -> SettingsState {
        let mut state = SettingsState {
            sub: SubPage::LabelerCatalog,
            ..Default::default()
        };
        state.labeler_catalog.snapshot = Some(LabelerCatalogSnapshot {
            entries,
            inspecting: None,
            error: None,
            loaded: true,
        });
        state
    }

    fn ids(els: &[Element]) -> Vec<String> {
        els.iter().map(|e| e.id.clone()).collect()
    }

    fn gesture(els: &[Element], id: &str, row: usize) -> Option<Action> {
        els.iter()
            .filter(|e| e.id == id)
            .find(|e| e.path == vec![("labeler-catalog-item".to_string(), row)])
            .and_then(|e| match &e.role {
                Role::Button(Gesture::Settings(a)) => Some(a.clone()),
                _ => None,
            })
    }

    /// Every static ui.yaml id of the catalog page paints, and one row per entry.
    #[test]
    fn the_catalog_paints_every_static_ui_yaml_id_and_one_row_per_entry() {
        let state = state_with(vec![entry("a", false), entry("b", true)]);
        let els = labeler_catalog_elements(&state);
        let ids = ids(&els);
        for id in [
            "page-heading",
            "labeler-catalog",
            "labeler-catalog-list",
            "settings-nav-back",
        ] {
            assert!(
                ids.contains(&id.to_string()),
                "missing {id:?}; have {ids:?}"
            );
        }
        assert_eq!(
            ids.iter().filter(|i| *i == "labeler-catalog-item").count(),
            2,
            "one row per catalog entry"
        );
        for id in [
            "labeler-catalog-item-publisher",
            "labeler-catalog-item-kind",
            "labeler-catalog-item-content-kind",
            "labeler-catalog-item-version",
            "labeler-catalog-item-factor",
        ] {
            assert_eq!(
                ids.iter().filter(|i| *i == id).count(),
                2,
                "one {id} per row"
            );
        }
        // The empty state is the honest alternative to rows, never both.
        assert!(!ids.contains(&"labeler-catalog-empty".to_string()));
    }

    /// The empty state paints only when the catalog is genuinely empty.
    #[test]
    fn an_empty_catalog_paints_its_empty_state() {
        let els = labeler_catalog_elements(&state_with(vec![]));
        let ids = ids(&els);
        assert!(ids.contains(&"labeler-catalog-empty".to_string()));
        assert!(!ids.contains(&"labeler-catalog-item".to_string()));
    }

    /// The three-state rule, on BOTH pages the one machine feeds
    /// (`docs/goal/ui/README.md` § *List pages: loading is not empty*).
    ///
    /// Zero rows is two different pictures, and only the loaded one may paint an
    /// empty state. Both unloaded shapes are covered: no snapshot folded at all
    /// (pre-login / before the nav-edge refresh returns), and a folded snapshot
    /// whose only read FAILED — the second is the one a `snapshot.is_some()`
    /// gate would have gotten wrong.
    #[test]
    fn neither_page_paints_its_empty_state_before_the_read_resolves() {
        let unloaded = |snapshot| {
            let mut state = SettingsState {
                sub: SubPage::LabelerCatalog,
                ..Default::default()
            };
            state.labeler_catalog.snapshot = snapshot;
            state
        };

        for (what, state) in [
            ("no snapshot folded yet", unloaded(None)),
            (
                "a snapshot whose only read failed",
                unloaded(Some(LabelerCatalogSnapshot {
                    entries: vec![],
                    inspecting: None,
                    error: Some(fauna_core::localized::LocalizedText::key("boom")),
                    loaded: false,
                })),
            ),
        ] {
            let catalog = ids(&labeler_catalog_elements(&state));
            assert!(
                !catalog.contains(&"labeler-catalog-empty".to_string()),
                "catalog page claimed 'nobody has published a labeler' with {what}"
            );
            let home = ids(&personalization_elements(&state));
            assert!(
                !home.contains(&"personalization-labelers-empty".to_string()),
                "home facet claimed 'you subscribe to nothing' with {what}"
            );
        }

        // The loading state paints no ID of its own: the ABSENCE of the empty
        // state beside zero rows is what identifies it, so the row containers
        // must still be there for a driver to read that absence against.
        let els = ids(&labeler_catalog_elements(&unloaded(None)));
        assert!(els.contains(&"labeler-catalog-list".to_string()));
        assert!(!els.contains(&"labeler-catalog-item".to_string()));
    }

    /// The load-bearing scoping contract: each row leaf's path **starts** with
    /// `labeler-catalog-item[row]`, which is what makes the shared action's
    /// single-step scoped read resolve at all.
    #[test]
    fn row_leaves_are_scoped_within_their_own_indexed_row() {
        let els = labeler_catalog_elements(&state_with(vec![entry("a", false), entry("b", true)]));
        for (id, row) in [
            ("labeler-catalog-item-publisher", 0usize),
            ("labeler-catalog-item-factor", 0),
            ("labeler-catalog-item-subscribe-button", 0),
            ("labeler-catalog-item-unsubscribe-button", 1),
        ] {
            let leaf = els
                .iter()
                .filter(|e| e.id == id)
                .find(|e| e.path == vec![("labeler-catalog-item".to_string(), row)]);
            assert!(leaf.is_some(), "{id} must scope to row {row}");
        }
        assert!(
            els.iter()
                .filter(|e| e.id == "labeler-catalog-item")
                .all(|e| e.path.is_empty()),
            "rows paint flat, not under labeler-catalog-list"
        );
    }

    /// Exactly one of subscribe/unsubscribe paints per row, gated on
    /// `entry.subscribed` — the pair the shared action polls to decide the
    /// round-trip landed. Both showing (or neither) would make that poll lie.
    #[test]
    fn exactly_one_of_subscribe_unsubscribe_paints_per_row() {
        let els = labeler_catalog_elements(&state_with(vec![entry("a", false), entry("b", true)]));
        for (row, subscribed) in [(0usize, false), (1, true)] {
            let has_sub = gesture(&els, "labeler-catalog-item-subscribe-button", row).is_some();
            let has_unsub = gesture(&els, "labeler-catalog-item-unsubscribe-button", row).is_some();
            assert_eq!(has_sub, !subscribed, "row {row}: subscribe gating");
            assert_eq!(has_unsub, subscribed, "row {row}: unsubscribe gating");
        }
    }

    /// **The index-mapping contract.** The personalization home paints a FILTERED
    /// list, so its painted row 0 is not the machine's entry 0. The gesture must
    /// carry the SNAPSHOT index or an unsubscribe from the home would drop
    /// whichever labeler happens to sit first in the catalog — a wrong, silent,
    /// user-visible mutation that no element-existence assertion would catch.
    #[test]
    fn the_home_dispatches_the_snapshot_index_not_the_painted_row() {
        // Two unsubscribed rows FIRST, so the only subscribed row is at snapshot
        // index 2 while it paints at home row 0.
        let state = state_with(vec![
            entry("a", false),
            entry("b", false),
            entry("c", true),
            entry("d", true),
        ]);
        let els = personalization_elements(&state);
        assert_eq!(
            els.iter()
                .filter(|e| e.id == "labeler-catalog-item")
                .count(),
            2,
            "the home paints only the SUBSCRIBED rows"
        );
        assert_eq!(
            gesture(&els, "labeler-catalog-item-unsubscribe-button", 0),
            Some(Action::LabelerUnsubscribe(2)),
            "home row 0 must dispatch snapshot index 2, not 0"
        );
        assert_eq!(
            gesture(&els, "labeler-catalog-item-unsubscribe-button", 1),
            Some(Action::LabelerUnsubscribe(3)),
        );
        // …while the catalog page, unfiltered, addresses row == index.
        let cat = labeler_catalog_elements(&state);
        assert_eq!(
            gesture(&cat, "labeler-catalog-item-unsubscribe-button", 2),
            Some(Action::LabelerUnsubscribe(2)),
        );
    }

    /// The home never offers inspect or subscribe — it is the "what I already
    /// granted" facet, and the shared test asserts their absence explicitly.
    #[test]
    fn the_home_offers_neither_inspect_nor_subscribe() {
        let els = personalization_elements(&state_with(vec![entry("a", true)]));
        let ids = ids(&els);
        for id in [
            "labeler-catalog-item-inspect-button",
            "labeler-catalog-item-subscribe-button",
        ] {
            assert!(!ids.contains(&id.to_string()), "the home painted {id}");
        }
        assert!(ids.contains(&"labeler-catalog-item-unsubscribe-button".to_string()));
    }

    /// The home's spine paints regardless of subscriptions, and its empty state
    /// appears only with none.
    #[test]
    fn the_home_paints_its_spine_and_gates_its_empty_state() {
        let empty = personalization_elements(&state_with(vec![entry("a", false)]));
        let spine = ids(&empty);
        for id in [
            "page-heading",
            "personalization",
            "personalization-feeds-link",
            "personalization-muted-words-link",
            "personalization-labelers-list",
            "personalization-browse-catalog-button",
            "settings-nav-back",
        ] {
            assert!(spine.contains(&id.to_string()), "missing {id:?}");
        }
        assert!(spine.contains(&"personalization-labelers-empty".to_string()));

        let filled = ids(&personalization_elements(&state_with(vec![entry(
            "a", true,
        )])));
        assert!(!filled.contains(&"personalization-labelers-empty".to_string()));
    }

    /// The inspect panel registers only while open, and its metadata carries the
    /// version + the client-side `verified` verdict — the two facts the trust gate
    /// exists to show.
    #[test]
    fn the_inspect_panel_registers_only_while_open() {
        let mut state = state_with(vec![entry("a", false)]);
        let closed = ids(&labeler_catalog_elements(&state));
        for id in [
            "labeler-inspect-panel",
            "labeler-inspect-metadata",
            "labeler-inspect-close-button",
        ] {
            assert!(
                !closed.contains(&id.to_string()),
                "{id} painted while closed"
            );
        }

        state.labeler_catalog.snapshot.as_mut().unwrap().inspecting = Some(inspect_view("wasm"));
        let els = labeler_catalog_elements(&state);
        let open = ids(&els);
        for id in [
            "labeler-inspect-panel",
            "labeler-inspect-metadata",
            "labeler-inspect-close-button",
        ] {
            assert!(open.contains(&id.to_string()), "missing {id} while open");
        }
        let metadata = &els
            .iter()
            .find(|e| e.id == "labeler-inspect-metadata")
            .unwrap()
            .text;
        assert!(metadata.contains("version: 7"), "metadata: {metadata:?}");
        assert!(
            metadata.contains("verified: true"),
            "metadata: {metadata:?}"
        );
    }

    fn inspect_view(kind: &str) -> LabelerInspectView {
        LabelerInspectView {
            labeler_id: "aa".repeat(32),
            version: 7,
            artifact_kind: kind.to_string(),
            wasm_hash: "bb".repeat(36),
            wasm_size: 2048,
            needs_text: true,
            needs_hashtags: false,
            needs_media_metadata: false,
            needs_author: false,
            needs_attachment_bytes: true,
            verified: true,
            list_name: Some("Cats".to_string()),
            list_entries: vec![
                LabelerInspectListEntry {
                    content_id: "cc".repeat(32),
                    score: 900,
                },
                LabelerInspectListEntry {
                    content_id: "dd".repeat(32),
                    score: 250,
                },
            ],
            // Both kinds' payloads are populated in the ONE fixture on purpose:
            // the panel must key off `artifact_kind`, never off "whichever
            // field happens to be non-empty" — a fixture that only ever fills
            // one at a time would pass either way.
            model_name: Some("Orange cats".to_string()),
            model_ngrams: vec![
                LabelerInspectModelNgram {
                    ngram: "orange cat".to_string(),
                    more: 4,
                    less: 0,
                },
                LabelerInspectModelNgram {
                    ngram: "tax advice".to_string(),
                    more: 1,
                    less: 3,
                },
            ],
        }
    }

    /// The list-kind section is `optional_elements`: present for a `list`
    /// artifact, absent for `wasm` — so a driver never reads an empty row set as
    /// a real (and wrong) "this list has no entries".
    #[test]
    fn the_list_kind_section_paints_only_for_a_list_artifact() {
        let mut state = state_with(vec![entry("a", false)]);
        state.labeler_catalog.snapshot.as_mut().unwrap().inspecting = Some(inspect_view("wasm"));
        let wasm = ids(&labeler_catalog_elements(&state));
        for id in [
            "labeler-inspect-list-name",
            "labeler-inspect-list-entry-count",
            "labeler-inspect-list-entries",
            "labeler-inspect-list-entry",
        ] {
            assert!(
                !wasm.contains(&id.to_string()),
                "{id} painted for a wasm labeler"
            );
        }

        state.labeler_catalog.snapshot.as_mut().unwrap().inspecting = Some(inspect_view("list"));
        let els = labeler_catalog_elements(&state);
        let list = ids(&els);
        for id in [
            "labeler-inspect-list-name",
            "labeler-inspect-list-entry-count",
            "labeler-inspect-list-entries",
        ] {
            assert!(
                list.contains(&id.to_string()),
                "missing {id} for a list labeler"
            );
        }
        assert_eq!(
            list.iter()
                .filter(|i| *i == "labeler-inspect-list-entry")
                .count(),
            2,
            "one row per list entry — the EXACT map, never a capped preview"
        );
        // The per-mille score rides through verbatim.
        let score = els
            .iter()
            .find(|e| {
                e.id == "labeler-inspect-list-entry-score"
                    && e.path == vec![("labeler-inspect-list-entry".to_string(), 0)]
            })
            .unwrap();
        assert_eq!(score.text, "900");
    }

    /// The text-model section is the List's twin, and the two are **mutually
    /// exclusive by KIND**: the fixture fills both payloads, so a panel that
    /// keyed off "whichever field is non-empty" would paint both sections here
    /// and this test is what catches it.
    #[test]
    fn the_model_kind_section_paints_only_for_a_text_model_artifact() {
        let mut state = state_with(vec![entry("a", false)]);
        for other in ["wasm", "list"] {
            state.labeler_catalog.snapshot.as_mut().unwrap().inspecting = Some(inspect_view(other));
            let painted = ids(&labeler_catalog_elements(&state));
            for id in [
                "labeler-inspect-model-name",
                "labeler-inspect-model-ngram-count",
                "labeler-inspect-model-entries",
                "labeler-inspect-model-entry",
            ] {
                assert!(
                    !painted.contains(&id.to_string()),
                    "{id} painted for a {other} labeler"
                );
            }
        }

        state.labeler_catalog.snapshot.as_mut().unwrap().inspecting =
            Some(inspect_view("text-model"));
        let els = labeler_catalog_elements(&state);
        let painted = ids(&els);
        for id in [
            "labeler-inspect-model-name",
            "labeler-inspect-model-ngram-count",
            "labeler-inspect-model-entries",
        ] {
            assert!(
                painted.contains(&id.to_string()),
                "missing {id} for a text-model labeler"
            );
        }
        assert!(
            !painted.contains(&"labeler-inspect-list-entry".to_string()),
            "the list section must not paint under a text-model artifact"
        );
        assert_eq!(
            painted
                .iter()
                .filter(|i| *i == "labeler-inspect-model-entry")
                .count(),
            2,
            "one row per n-gram — the FULL vocabulary, never a capped preview"
        );
    }

    /// The subscriber reads the same two shared faces the publisher's review
    /// rows painted, so a dislike-dominant pattern says so here too and the
    /// count stays class-blind. This is the "inspect before you subscribe"
    /// promise: what the publisher was shown is what you are shown.
    #[test]
    fn the_model_rows_carry_the_shared_direction_and_class_blind_count() {
        let mut state = state_with(vec![entry("a", false)]);
        state.labeler_catalog.snapshot.as_mut().unwrap().inspecting =
            Some(inspect_view("text-model"));
        let els = labeler_catalog_elements(&state);
        let field = |id: &str, row: usize| {
            els.iter()
                .find(|e| {
                    e.id == id && e.path == vec![("labeler-inspect-model-entry".to_string(), row)]
                })
                .unwrap_or_else(|| panic!("{id} row {row}"))
                .text
                .clone()
        };
        assert_eq!(field("labeler-inspect-model-entry-text", 0), "orange cat");
        assert_eq!(field("labeler-inspect-model-entry-text", 1), "tax advice");
        assert_ne!(
            field("labeler-inspect-model-entry-direction", 0),
            field("labeler-inspect-model-entry-direction", 1),
            "a like-dominant and a dislike-dominant pattern must not read alike"
        );
        // 1 + 3 = 4, never the dominant class's 3: the sum is the quantity the
        // 3-post privacy floor bounds.
        assert!(
            field("labeler-inspect-model-entry-count", 1).contains('4'),
            "the inspect count must be class-blind too"
        );
    }

    #[test]
    fn a_text_model_this_build_cannot_score_says_so_on_its_kind_badge() {
        // The "and says so" half of the unknown-version contract: the compose
        // seam has already left this factor INERT, and the catalog row is the
        // only place the user is told why (a feed banner would be wrong — the
        // feed is correct, just missing a factor).
        let state = state_with(vec![model_entry(
            "aa",
            fauna_core::scoring::TEXT_MODEL_ARTIFACT_VERSION as u64 + 3,
        )]);
        let els = labeler_catalog_elements(&state);
        let badge = els
            .iter()
            .find(|e| e.id == "labeler-catalog-item-kind")
            .expect("every catalog row paints a kind badge");
        assert_eq!(
            badge.text,
            t::KIND_NEEDS_NEWER_APP,
            "an unscorable text-model must not silently render as an ordinary one"
        );
    }

    #[test]
    fn an_ordinary_text_model_still_renders_the_raw_kind() {
        // ui.yaml pins this element to the raw discriminator, and both tier_3
        // journeys assert `== "text-model"` on it — so the override must be
        // confined to the unsupported case and must not leak into the ordinary
        // one, nor into a row from a nest too old to state a version (0).
        for version in [0, fauna_core::scoring::TEXT_MODEL_ARTIFACT_VERSION as u64] {
            let state = state_with(vec![model_entry("bb", version)]);
            let els = labeler_catalog_elements(&state);
            let badge = els
                .iter()
                .find(|e| e.id == "labeler-catalog-item-kind")
                .expect("every catalog row paints a kind badge");
            assert_eq!(
                badge.text, "text-model",
                "artifact_version {version} must render the raw kind"
            );
        }
    }
}
