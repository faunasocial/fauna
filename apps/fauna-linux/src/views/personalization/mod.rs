//! Settings → **Personalization** (the unified home) + Settings → **Community
//! labelers** (the catalog) — two settings sub-pages backed by **one** shared
//! `LabelerCatalogMachine` and **one** observer-driven render loop. Mirrors
//! `views/devices_folders/mod.rs` (two sub-pages, one shared machine).
//!
//! `docs/goal/architecture/content-moderation-and-ranking.md` § Composition +
//! § Tier-3 community models. The personalization home hubs three facets
//! WITHOUT rebuilding them: **Feeds** (a link out to the feed page's
//! create-feed dialog — the trainable factor-weight authoring lands once Slice 1 ships), **Muted words** (a link
//! re-homing the ALREADY-SHIPPED `settings::muted_words` sub-page — reused,
//! not rebuilt), and **Community labelers** (the caller's SUBSCRIBED tier-3
//! labelers rendered inline, each unsubscribable, with a link out to the full
//! **Community labelers** catalog sub-page for browse + inspect-before-
//! subscribe + subscribe).

mod publish_sheet;
mod trained_topics;

use fauna_ui_ids as ids;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;

use fauna_labeler_catalog_machine::{
    LabelerCatalogEntry, LabelerCatalogMachine, LabelerCatalogObserver, LabelerInspectView,
};

use fauna_client_moderation::moderation::ModerationSignalShareStatusReply;

use crate::async_helper::spawn_with_snapshot;
use crate::client::FaunaClient;
use crate::i18n::strings;
use crate::i18n::strings::{labeler_catalog, personalization};
use crate::testid::set_test_id;

/// Handles the two sub-pages hand back to the settings shell.
pub struct PersonalizationHandles {
    /// The shared-Rust `LabelerCatalogMachine`. Refreshed on auth + whenever
    /// either sub-page becomes visible.
    pub labeler_catalog_machine: Arc<LabelerCatalogMachine>,
}

/// Bridges `LabelerCatalogMachine` notifications to the GTK main loop. Mirrors
/// `GtkDevicesObserver` / `GtkMediaObserver`.
struct GtkLabelerCatalogObserver {
    tx: async_channel::Sender<()>,
}

impl LabelerCatalogObserver for GtkLabelerCatalogObserver {
    fn on_changed(&self) {
        let _ = self.tx.try_send(());
    }
}

/// Build the shared machine **with its grant seams**: subscribing a `wasm`
/// mail labeler mints the per-labeler grant to this nest's mail service and
/// unsubscribing revokes it (`content-moderation-and-ranking.md` § Tier-3 →
/// *Subscribing = minting a capability*), which needs the actor's identity key
/// — tui's `LabelerCatalogState::build`, lifted. A `secret_hex` that will not
/// decode (the launch flow validated it to reach Online, so this is an
/// unrecoverable identity fault) falls back to the grant-less machine with a
/// warning, so the pages still browse, inspect and (un)subscribe — the Nests
/// page's own fallback (`mail_glue::build_linked_nests_machine_with_mail_relay_and_trust`).
fn build_machine(
    fauna_client: &FaunaClient,
    observer: Arc<dyn LabelerCatalogObserver>,
) -> Arc<LabelerCatalogMachine> {
    let nest = Arc::clone(fauna_client.nest_rpc());
    match fauna_core::identity::ActorKeypair::from_secret_hex(fauna_client.secret_hex()) {
        Ok(keypair) => fauna_labeler_catalog_machine::build_labeler_catalog_machine_with_grants(
            nest,
            keypair,
            crate::account_runtime::ledger_seam(),
            crate::account_runtime::mail_store(),
            observer,
        ),
        Err(e) => {
            tracing::warn!(
                "labeler catalog: decode secret_hex: {e}; building without grant seams — \
                 a mail labeler subscription will not mint its grant"
            );
            fauna_labeler_catalog_machine::build_labeler_catalog_machine(nest, observer)
        }
    }
}

fn page_heading(text: &str) -> gtk::Label {
    let heading = gtk::Label::builder()
        .label(text)
        .halign(gtk::Align::Start)
        .css_classes(["title-1"])
        .build();
    set_test_id(&heading, ids::PAGE_HEADING);
    heading
}

fn error_label() -> gtk::Label {
    let label = gtk::Label::builder().visible(false).build();
    label.add_css_class("error");
    set_test_id(&label, ids::ERROR_MESSAGE);
    label
}

/// The **Personalization** home sub-page static shell.
struct HomeShell {
    content: gtk::Box,
    feeds_link: gtk::Button,
    muted_words_link: gtk::Button,
    labelers_list: gtk::ListBox,
    labelers_empty: gtk::Label,
    browse_catalog_button: gtk::Button,
    clear_engagement_data_button: gtk::Button,
    // Layer-B signal-sharing (engagement-cues.md § Layer B): the opt-in toggle +
    // the "what this nest publishes" transparency pane. Mirrors the report-share
    // pane in `settings/mail_spam.rs`, on the Personalization home instead.
    share_signals_toggle: gtk::Switch,
    signal_published_group: adw::PreferencesGroup,
    signal_published_placeholder: adw::ActionRow,
    signal_published_rows: Rc<std::cell::RefCell<Vec<adw::ActionRow>>>,
    error_label: gtk::Label,
}

/// Returns the shell plus the Trained-topics facet's widget handles (already
/// appended into `content`; handed separately because `trained_topics::wire`
/// takes them by value while the shell moves into the render ctx).
fn build_home_shell() -> (HomeShell, trained_topics::TrainedTopicsWidgets) {
    let content = crate::views::layout::page_box(16);
    content.append(&page_heading(personalization::TITLE));
    let err = error_label();
    content.append(&err);

    let feeds_link = gtk::Button::with_label(personalization::FEEDS_LINK);
    set_test_id(&feeds_link, ids::PERSONALIZATION_FEEDS_LINK);
    content.append(&feeds_link);

    let muted_words_link = gtk::Button::with_label(personalization::MUTED_WORDS_LINK);
    set_test_id(&muted_words_link, ids::PERSONALIZATION_MUTED_WORDS_LINK);
    content.append(&muted_words_link);

    let labelers_group = adw::PreferencesGroup::builder()
        .title(labeler_catalog::TITLE)
        .build();
    let labelers_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    set_test_id(&labelers_list, ids::PERSONALIZATION_LABELERS_LIST);
    labelers_group.add(&labelers_list);
    content.append(&labelers_group);

    let labelers_empty = gtk::Label::new(Some(personalization::LABELERS_EMPTY));
    labelers_empty.add_css_class("dim-label");
    labelers_empty.set_halign(gtk::Align::Start);
    set_test_id(&labelers_empty, ids::PERSONALIZATION_LABELERS_EMPTY);
    content.append(&labelers_empty);

    let browse_catalog_button = gtk::Button::with_label(personalization::BROWSE_CATALOG);
    browse_catalog_button.set_halign(gtk::Align::Start);
    set_test_id(
        &browse_catalog_button,
        ids::PERSONALIZATION_BROWSE_CATALOG_BUTTON,
    );
    content.append(&browse_catalog_button);

    // Trained topics facet (topic-factors.md § Authoring surface) — the
    // trainable tier-1 factors' CRUD home, after the labelers facet per the
    // ui.yaml `personalization` page order.
    let trained = trained_topics::build_widgets();
    content.append(&trained.group);

    // "Clear activity data" (engagement-cues.md § At rest): the user-revocable
    // affordance for the sealed engagement-cue rollup — deletes cues:v1 from
    // the user's own nest and resets the live engine.
    let clear_engagement_data_button =
        gtk::Button::with_label(personalization::CLEAR_ENGAGEMENT_DATA);
    clear_engagement_data_button.set_halign(gtk::Align::Start);
    set_test_id(
        &clear_engagement_data_button,
        ids::PERSONALIZATION_CLEAR_ENGAGEMENT_DATA_BUTTON,
    );
    crate::offline_gate::declare_wire_kind(
        &clear_engagement_data_button,
        "fauna.personalization.model.delete",
    );
    content.append(&clear_engagement_data_button);

    // "Share anonymous signals" (engagement-cues.md § Layer B): the opt-in for
    // contributing k-anonymized per-item verdicts to the network — default off,
    // the sibling of mail-spam's report-sharing toggle. A false→true flip lets
    // this device's derived cue verdicts on PUBLIC posts join the shared
    // aggregate; true→false also withdraws every `signal:*` row this actor
    // contributed (the nest opt-out sweep), so the published list may shrink.
    let share_signals_group = adw::PreferencesGroup::builder()
        .title(personalization::SHARE_SIGNALS_TITLE)
        .build();
    let share_signals_toggle = gtk::Switch::builder()
        .valign(gtk::Align::Center)
        .active(false)
        .build();
    set_test_id(
        &share_signals_toggle,
        ids::PERSONALIZATION_SHARE_SIGNALS_TOGGLE,
    );
    crate::offline_gate::declare_wire_kind(
        &share_signals_toggle,
        "fauna.moderation.signal_share.set",
    );
    let share_signals_row = adw::ActionRow::builder()
        .title(personalization::SHARE_SIGNALS_LABEL)
        .subtitle(personalization::SHARE_SIGNALS_SUBTITLE)
        .activatable(false)
        .build();
    share_signals_row.add_suffix(&share_signals_toggle);
    share_signals_group.add(&share_signals_row);
    content.append(&share_signals_group);

    // signal-share-published-list — the ≥k aggregates this nest exports to peers
    // (byte-identical to the federation export — the transparency guarantee;
    // nest-wide, so it carries `report:*` and `signal:*` alike). Rows
    // (signal-share-published-list-item*) are rebuilt from the status reply on
    // every render_signal_share(). Empty on a fresh nest.
    let signal_published_group = adw::PreferencesGroup::builder()
        .title(personalization::SIGNAL_PUBLISHED_TITLE)
        .description(personalization::SIGNAL_PUBLISHED_DESCRIPTION)
        .build();
    signal_published_group.set_header_suffix(Some(&crate::settings::marker(
        "signal-share-published-list",
    )));
    let signal_published_placeholder = adw::ActionRow::builder()
        .title(personalization::SIGNAL_PUBLISHED_EMPTY)
        .build();
    signal_published_group.add(&signal_published_placeholder);
    content.append(&signal_published_group);

    (
        HomeShell {
            content,
            feeds_link,
            muted_words_link,
            labelers_list,
            labelers_empty,
            browse_catalog_button,
            clear_engagement_data_button,
            share_signals_toggle,
            signal_published_group,
            signal_published_placeholder,
            signal_published_rows: Rc::new(std::cell::RefCell::new(Vec::new())),
            error_label: err,
        },
        trained,
    )
}

/// The **Community labelers** catalog sub-page static shell.
struct CatalogShell {
    content: gtk::Box,
    catalog_list: gtk::ListBox,
    catalog_empty: gtk::Label,
    inspect_panel: gtk::Box,
    inspect_metadata: gtk::Label,
    /// The list-kind inspect section (hidden for `wasm` labelers): the decoded
    /// publisher-chosen name, the entry count, and the exact id→score rows —
    /// the frame's "renders the exact id→score map before subscribing".
    inspect_list_name: gtk::Label,
    inspect_list_entry_count: gtk::Label,
    inspect_list_entries: gtk::ListBox,
    /// The text-model-kind inspect section (hidden for every other kind): the
    /// decoded publisher-chosen name, the n-gram count, and the FULL
    /// vocabulary — the model's twin of the list section above.
    inspect_model_name: gtk::Label,
    inspect_model_ngram_count: gtk::Label,
    inspect_model_entries: gtk::ListBox,
    inspect_close_button: gtk::Button,
    error_label: gtk::Label,
}

fn build_catalog_shell() -> CatalogShell {
    let content = crate::views::layout::page_box(16);
    content.append(&page_heading(labeler_catalog::TITLE));
    let err = error_label();
    content.append(&err);

    let inspect_panel = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .visible(false)
        .build();
    set_test_id(&inspect_panel, ids::LABELER_INSPECT_PANEL);
    let inspect_metadata = gtk::Label::builder()
        .wrap(true)
        .halign(gtk::Align::Start)
        .build();
    set_test_id(&inspect_metadata, ids::LABELER_INSPECT_METADATA);
    inspect_panel.append(&inspect_metadata);

    // List-kind section (ui.yaml optional_elements — visible only when the
    // inspected labeler is a `list` artifact).
    let inspect_list_name = gtk::Label::builder()
        .wrap(true)
        .halign(gtk::Align::Start)
        .visible(false)
        .build();
    set_test_id(&inspect_list_name, ids::LABELER_INSPECT_LIST_NAME);
    inspect_panel.append(&inspect_list_name);
    let inspect_list_entry_count = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .visible(false)
        .build();
    inspect_list_entry_count.add_css_class("dim-label");
    set_test_id(
        &inspect_list_entry_count,
        ids::LABELER_INSPECT_LIST_ENTRY_COUNT,
    );
    inspect_panel.append(&inspect_list_entry_count);
    let inspect_list_entries = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .visible(false)
        .build();
    set_test_id(&inspect_list_entries, ids::LABELER_INSPECT_LIST_ENTRIES);
    inspect_panel.append(&inspect_list_entries);

    // Text-model-kind section (ui.yaml optional_elements — visible only when
    // the inspected labeler is a `text-model` artifact), the list section's
    // twin.
    let inspect_model_name = gtk::Label::builder()
        .wrap(true)
        .halign(gtk::Align::Start)
        .visible(false)
        .build();
    set_test_id(&inspect_model_name, ids::LABELER_INSPECT_MODEL_NAME);
    inspect_panel.append(&inspect_model_name);
    let inspect_model_ngram_count = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .visible(false)
        .build();
    inspect_model_ngram_count.add_css_class("dim-label");
    set_test_id(
        &inspect_model_ngram_count,
        ids::LABELER_INSPECT_MODEL_NGRAM_COUNT,
    );
    inspect_panel.append(&inspect_model_ngram_count);
    let inspect_model_entries = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .visible(false)
        .build();
    set_test_id(&inspect_model_entries, ids::LABELER_INSPECT_MODEL_ENTRIES);
    inspect_panel.append(&inspect_model_entries);

    let inspect_close_button = gtk::Button::with_label(labeler_catalog::CLOSE_INSPECT);
    inspect_close_button.set_halign(gtk::Align::Start);
    set_test_id(&inspect_close_button, ids::LABELER_INSPECT_CLOSE_BUTTON);
    inspect_panel.append(&inspect_close_button);
    content.append(&inspect_panel);

    let catalog_group = adw::PreferencesGroup::builder()
        .title(labeler_catalog::TITLE)
        .build();
    let catalog_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    set_test_id(&catalog_list, ids::LABELER_CATALOG_LIST);
    catalog_group.add(&catalog_list);
    content.append(&catalog_group);

    let catalog_empty = gtk::Label::new(Some(labeler_catalog::EMPTY));
    catalog_empty.add_css_class("dim-label");
    catalog_empty.set_halign(gtk::Align::Start);
    set_test_id(&catalog_empty, ids::LABELER_CATALOG_EMPTY);
    content.append(&catalog_empty);

    CatalogShell {
        content,
        catalog_list,
        catalog_empty,
        inspect_panel,
        inspect_metadata,
        inspect_list_name,
        inspect_list_entry_count,
        inspect_list_entries,
        inspect_model_name,
        inspect_model_ngram_count,
        inspect_model_entries,
        inspect_close_button,
        error_label: err,
    }
}

/// Build one `labeler-catalog-item` row. `show_inspect_subscribe` gates the
/// inspect + subscribe affordances (labeler-catalog page only — ui.yaml
/// `labeler-catalog-item`: "labeler-catalog-item-subscribe-button …
/// labeler-catalog only"); unsubscribe is always present so the
/// personalization home's subscribed-labelers facet can un-subscribe inline.
fn build_labeler_row(
    machine: &Arc<LabelerCatalogMachine>,
    runtime: &tokio::runtime::Handle,
    entry: &LabelerCatalogEntry,
    index: u32,
    show_inspect_subscribe: bool,
) -> gtk::Box {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&row, ids::LABELER_CATALOG_ITEM);

    let info = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .hexpand(true)
        .build();

    let publisher = gtk::Label::new(Some(&entry.publisher_actor));
    publisher.set_halign(gtk::Align::Start);
    set_test_id(&publisher, ids::LABELER_CATALOG_ITEM_PUBLISHER);
    info.append(&publisher);

    // The artifact kind (`list` | `wasm` | `text-model`, normalized by the
    // machine) — a curated List is distinguishable from an executable module
    // BEFORE inspect (frame § Tier-3 artifact kinds). Rendered verbatim,
    // EXCEPT the one case a subscribed `text-model` whose tokenizer contract
    // this build does not implement: the compose seam leaves that factor
    // inert, and this row is where the user learns why. The predicate and
    // wording are both shared (`text_model_needs_newer_app`, over the same
    // `scoring::text_model_version_supported` the seam's inert branch
    // reads), so this badge cannot disagree with the scorer.
    let kind_text =
        crate::i18n::text_model_needs_newer_app(&entry.artifact_kind, entry.artifact_version)
            .unwrap_or_else(|| entry.artifact_kind.clone());
    let kind = gtk::Label::new(Some(&kind_text));
    kind.set_halign(gtk::Align::Start);
    set_test_id(&kind, ids::LABELER_CATALOG_ITEM_KIND);
    info.append(&kind);

    let content_kind = gtk::Label::new(Some(&entry.content_kind));
    content_kind.set_halign(gtk::Align::Start);
    set_test_id(&content_kind, ids::LABELER_CATALOG_ITEM_CONTENT_KIND);
    info.append(&content_kind);

    let version = gtk::Label::new(Some(&entry.version.to_string()));
    version.set_halign(gtk::Align::Start);
    set_test_id(&version, ids::LABELER_CATALOG_ITEM_VERSION);
    info.append(&version);

    let factor = gtk::Label::new(Some(&entry.factor));
    factor.set_halign(gtk::Align::Start);
    set_test_id(&factor, ids::LABELER_CATALOG_ITEM_FACTOR);
    info.append(&factor);

    row.append(&info);

    if show_inspect_subscribe {
        let inspect = gtk::Button::with_label(labeler_catalog::INSPECT);
        set_test_id(&inspect, ids::LABELER_CATALOG_ITEM_INSPECT_BUTTON);
        crate::offline_gate::declare_wire_kind(&inspect, "fauna.labelers.inspect");
        {
            let machine = Arc::clone(machine);
            let runtime = runtime.clone();
            inspect.connect_clicked(move |_| {
                let machine = Arc::clone(&machine);
                runtime.spawn(async move { machine.inspect(index).await });
            });
        }
        row.append(&inspect);

        if !entry.subscribed {
            let subscribe = gtk::Button::with_label(labeler_catalog::SUBSCRIBE);
            subscribe.add_css_class("suggested-action");
            set_test_id(&subscribe, ids::LABELER_CATALOG_ITEM_SUBSCRIBE_BUTTON);
            crate::offline_gate::declare_wire_kind(&subscribe, "fauna.labelers.subscribe");
            {
                let machine = Arc::clone(machine);
                let runtime = runtime.clone();
                subscribe.connect_clicked(move |_| {
                    let machine = Arc::clone(&machine);
                    runtime.spawn(async move { machine.subscribe(index).await });
                });
            }
            row.append(&subscribe);
        }
    }

    if entry.subscribed {
        let unsubscribe = gtk::Button::with_label(labeler_catalog::UNSUBSCRIBE);
        set_test_id(&unsubscribe, ids::LABELER_CATALOG_ITEM_UNSUBSCRIBE_BUTTON);
        crate::offline_gate::declare_wire_kind(&unsubscribe, "fauna.labelers.unsubscribe");
        {
            let machine = Arc::clone(machine);
            let runtime = runtime.clone();
            unsubscribe.connect_clicked(move |_| {
                let machine = Arc::clone(&machine);
                runtime.spawn(async move { machine.unsubscribe(index).await });
            });
        }
        row.append(&unsubscribe);
    }

    row
}

/// Rebuild a `ListBox`'s rows from a filtered, indexed view of the catalog.
/// `indices` pairs each shown entry with its ORIGINAL snapshot index (the
/// machine's `inspect`/`subscribe`/`unsubscribe` gestures are index-addressed
/// into the full, unfiltered `entries`), so the personalization home's
/// subscribed-only filter doesn't scramble which row a gesture targets.
fn rebuild_rows(
    list: &gtk::ListBox,
    machine: &Arc<LabelerCatalogMachine>,
    runtime: &tokio::runtime::Handle,
    indices: &[(u32, &LabelerCatalogEntry)],
    show_inspect_subscribe: bool,
) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    for (index, entry) in indices {
        let row = build_labeler_row(machine, runtime, entry, *index, show_inspect_subscribe);
        list.append(&row);
    }
}

/// Render the inspect panel from the snapshot's `inspecting` view, if any.
/// For a `list` artifact the panel additionally renders the decoded
/// publisher-chosen name + the **exact** id→score map (frame § Tier-3 artifact
/// kinds — every entry, never a capped preview: a truncated map is not the
/// exact map).
fn render_inspect_panel(shell: &CatalogShell, inspecting: Option<&LabelerInspectView>) {
    while let Some(child) = shell.inspect_list_entries.first_child() {
        shell.inspect_list_entries.remove(&child);
    }
    while let Some(child) = shell.inspect_model_entries.first_child() {
        shell.inspect_model_entries.remove(&child);
    }
    match inspecting {
        Some(view) => {
            let text = format!(
                "labeler_id: {}\nversion: {}\nartifact_kind: {}\nwasm_hash: {}\nwasm_size: {}\nneeds_text: {}\nneeds_hashtags: {}\nneeds_media_metadata: {}\nneeds_author: {}\nneeds_attachment_bytes: {}\nverified: {}",
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
            );
            shell.inspect_metadata.set_text(&text);

            let is_list = view.artifact_kind == "list";
            shell.inspect_list_name.set_visible(is_list);
            shell.inspect_list_entry_count.set_visible(is_list);
            shell.inspect_list_entries.set_visible(is_list);
            if is_list {
                // An unnamed list is valid (pre-name artifacts) — say so rather
                // than rendering an empty label.
                shell.inspect_list_name.set_text(&match &view.list_name {
                    Some(name) => labeler_catalog::list_name(name),
                    None => labeler_catalog::UNNAMED_LIST.to_string(),
                });
                shell
                    .inspect_list_entry_count
                    .set_text(&labeler_catalog::list_entry_count(
                        &view.list_entries.len().to_string(),
                    ));
                for entry in &view.list_entries {
                    let row = gtk::Box::builder()
                        .orientation(gtk::Orientation::Horizontal)
                        .spacing(8)
                        .accessible_role(gtk::AccessibleRole::Group)
                        .build();
                    set_test_id(&row, ids::LABELER_INSPECT_LIST_ENTRY);

                    let id = gtk::Label::new(Some(&entry.content_id));
                    id.set_hexpand(true);
                    id.set_halign(gtk::Align::Start);
                    id.set_xalign(0.0);
                    id.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
                    set_test_id(&id, ids::LABELER_INSPECT_LIST_ENTRY_ID);
                    row.append(&id);

                    // The publisher's per-mille verbatim — the same number the
                    // publish sheet showed; no rescale anywhere in the chain.
                    let score = gtk::Label::new(Some(&entry.score.to_string()));
                    score.add_css_class("dim-label");
                    set_test_id(&score, ids::LABELER_INSPECT_LIST_ENTRY_SCORE);
                    row.append(&score);

                    shell.inspect_list_entries.append(&row);
                }
            }

            // The text-model section, the same optional_elements shape: it
            // registers only for a `text-model` artifact, keyed on the KIND
            // rather than on the payload being non-empty — an empty
            // vocabulary is a decode failure, not a reason to render the
            // panel as if it were some other kind.
            let is_model = view.artifact_kind == fauna_core::scoring::artifact_kind::TEXT_MODEL;
            shell.inspect_model_name.set_visible(is_model);
            shell.inspect_model_ngram_count.set_visible(is_model);
            shell.inspect_model_entries.set_visible(is_model);
            if is_model {
                shell.inspect_model_name.set_text(&match &view.model_name {
                    Some(name) => labeler_catalog::model_name(name),
                    None => labeler_catalog::UNNAMED_MODEL.to_string(),
                });
                shell
                    .inspect_model_ngram_count
                    .set_text(&labeler_catalog::model_ngram_count(
                        &view.model_ngrams.len().to_string(),
                    ));
                // EVERY entry — the vocabulary is the model's whole matching
                // surface, and a capped preview is not the artifact.
                for entry in &view.model_ngrams {
                    let row = gtk::Box::builder()
                        .orientation(gtk::Orientation::Horizontal)
                        .spacing(8)
                        .accessible_role(gtk::AccessibleRole::Group)
                        .build();
                    set_test_id(&row, ids::LABELER_INSPECT_MODEL_ENTRY);

                    let text = gtk::Label::new(Some(&entry.ngram));
                    text.set_hexpand(true);
                    text.set_halign(gtk::Align::Start);
                    text.set_xalign(0.0);
                    text.set_ellipsize(gtk::pango::EllipsizeMode::End);
                    set_test_id(&text, ids::LABELER_INSPECT_MODEL_ENTRY_TEXT);
                    row.append(&text);

                    // The SAME two shared faces the publisher's review rows
                    // painted, so what a publisher was shown before
                    // publishing is what a subscriber reads before
                    // subscribing.
                    let direction = gtk::Label::new(Some(&crate::i18n::ngram_direction(
                        entry.more, entry.less,
                    )));
                    direction.add_css_class("dim-label");
                    set_test_id(&direction, ids::LABELER_INSPECT_MODEL_ENTRY_DIRECTION);
                    row.append(&direction);

                    let count = gtk::Label::new(Some(&crate::i18n::ngram_doc_count(
                        entry.more, entry.less,
                    )));
                    count.add_css_class("dim-label");
                    set_test_id(&count, ids::LABELER_INSPECT_MODEL_ENTRY_COUNT);
                    row.append(&count);

                    shell.inspect_model_entries.append(&row);
                }
            }
            shell.inspect_panel.set_visible(true);
        }
        None => shell.inspect_panel.set_visible(false),
    }
}

/// Widgets + state the render loop rewrites on every observer tick.
struct RenderCtx {
    home: HomeShell,
    catalog: CatalogShell,
}

/// Everything the Layer-B signal-sharing pane's handlers + render need. Holds
/// cloned widget handles (GTK objects are refcounted, so this shares the same
/// widgets) plus the echo-suppression flag, so it lives independently of the
/// `RenderCtx` the labeler loop owns.
struct SignalShareCtx {
    toggle: gtk::Switch,
    published_group: adw::PreferencesGroup,
    published_placeholder: adw::ActionRow,
    published_rows: Rc<std::cell::RefCell<Vec<adw::ActionRow>>>,
    error_label: gtk::Label,
    /// Set while `render_signal_share` programmatically updates the toggle, so its
    /// `active_notify` handler doesn't echo the change back as a `set`.
    syncing: Cell<bool>,
    rt: tokio::runtime::Handle,
}

/// Wire the signal-sharing opt-in toggle + transparency pane over the shared
/// `FeedManager` (engagement-cues.md § Layer B). Mirrors the report-share pane in
/// `settings/mail_spam.rs`: hydrate on build + on page-visible, flip on toggle
/// (echo-suppressed), rebuild the published list from the nest-confirmed reply.
fn wire_signal_sharing(
    home: &HomeShell,
    fauna_client: &Rc<FaunaClient>,
    home_page: &gtk::ScrolledWindow,
) {
    let ctx = Rc::new(SignalShareCtx {
        toggle: home.share_signals_toggle.clone(),
        published_group: home.signal_published_group.clone(),
        published_placeholder: home.signal_published_placeholder.clone(),
        published_rows: Rc::clone(&home.signal_published_rows),
        error_label: home.error_label.clone(),
        syncing: Cell::new(false),
        rt: fauna_client.runtime_handle(),
    });

    hydrate_signal_share(&ctx);

    // Toggle → FeedManager::set_signal_sharing (skip the echo from
    // render_signal_share). A false→true flip opts in; true→false ALSO withdraws
    // every signal this actor contributed (the nest opt-out sweep), so the
    // published list may shrink on the re-read.
    {
        let ctx = Rc::clone(&ctx);
        home.share_signals_toggle.connect_active_notify(move |sw| {
            if ctx.syncing.get() {
                return;
            }
            set_signal_share(&ctx, sw.is_active());
        });
    }

    // Refresh on page-visible: the opt-in + published list change out-of-band
    // (another user on this nest crossing k; this actor opting out on another
    // device), and the manager may not exist yet at build time. Mirrors the
    // labeler/trained refresh-on-map.
    {
        let ctx = Rc::clone(&ctx);
        home_page.connect_map(move |_| hydrate_signal_share(&ctx));
    }
}

/// Read the signal opt-in + published list over the shared `FeedManager`
/// (`crate::feed::host::manager()` — the same accessor the clear-activity-data
/// button uses) and render it. `None` (pre-auth: the page mounts at app init)
/// is a no-op — the page-visible refresh re-hydrates once the manager exists.
/// `FeedManager::signal_share_status` is a single NestClient RPC (plus a
/// trivial local cache write) — the transport already parks it while the
/// socket comes up (transport.md § Request lifecycle step 3).
fn hydrate_signal_share(ctx: &Rc<SignalShareCtx>) {
    let Some(manager) = crate::feed::host::manager() else {
        return;
    };
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move { manager.signal_share_status().await },
        move |res| render_signal_share(&ctx_render, res),
    );
}

/// Set the opt-in (`FeedManager::set_signal_sharing` → `signal_share.set`), then
/// re-read status so the toggle + published list reflect the persisted value
/// (opting out withdraws this actor's signals, which may shrink the list).
fn set_signal_share(ctx: &Rc<SignalShareCtx>, share: bool) {
    let Some(manager) = crate::feed::host::manager() else {
        crate::settings::render_error_label(&ctx.error_label, Some(strings::common::NOT_CONNECTED));
        return;
    };
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move { manager.set_signal_sharing(share).await },
        move |res| render_signal_share(&ctx_render, res),
    );
}

/// Render a signal-share status reply: reflect the opt-in toggle (echo-suppressed)
/// and rebuild the published list. An error surfaces via the page `error-message`.
/// Delegates to the shared `render_share_pane` (round 195 of the shared-Rust
/// harvest sweep).
fn render_signal_share(
    ctx: &Rc<SignalShareCtx>,
    res: Result<ModerationSignalShareStatusReply, String>,
) {
    crate::settings::render_share_pane(
        crate::settings::SharePaneWidgets {
            toggle: &ctx.toggle,
            syncing: &ctx.syncing,
            error_label: &ctx.error_label,
            published_group: &ctx.published_group,
            published_rows: &ctx.published_rows,
            published_placeholder: &ctx.published_placeholder,
        },
        "signal-share",
        personalization::SIGNAL_PUBLISHED_CONTRIBUTORS,
        res.map(|s| (s.share, s.published)),
    );
}

/// Build the **Personalization** home + **Community labelers** catalog
/// sub-pages over one shared `LabelerCatalogMachine`. Returns
/// `(home_page, catalog_page, handles)` — each page is a `gtk::ScrolledWindow`
/// the settings shell adds to its sub-stack. `on_navigate_to_feed` exits
/// Settings to the top-level Feed page (the "Feeds" facet link);
/// `on_navigate_to_muted_words` / `on_navigate_to_catalog` switch the
/// SETTINGS sub-stack to the already-shipped `muted-words` sub-page / this
/// module's own `labeler-catalog` sub-page (both same-stack, wired by the
/// caller since only it holds the settings sub-stack at construction time).
#[allow(clippy::type_complexity)]
pub fn build_personalization_and_catalog_pages(
    fauna_client: &Rc<FaunaClient>,
    on_navigate_to_feed: Rc<dyn Fn()>,
    on_navigate_to_muted_words: Rc<dyn Fn()>,
    on_navigate_to_catalog: Rc<dyn Fn()>,
) -> (
    gtk::ScrolledWindow,
    gtk::ScrolledWindow,
    PersonalizationHandles,
) {
    let (home, trained_widgets) = build_home_shell();
    let catalog = build_catalog_shell();

    let home_page = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .child(&home.content)
        .build();
    let catalog_page = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .child(&catalog.content)
        .build();

    home.feeds_link
        .connect_clicked(move |_| on_navigate_to_feed());
    home.muted_words_link
        .connect_clicked(move |_| on_navigate_to_muted_words());
    home.browse_catalog_button
        .connect_clicked(move |_| on_navigate_to_catalog());

    // Clear activity data → FeedManager::delete_cue_rollup (model.delete on
    // cues:v1 + live-engine reset). The manager is built on auth; `None`
    // (pre-auth) surfaces on the page error element rather than panicking.
    {
        let error_label = home.error_label.clone();
        let runtime = fauna_client.runtime_handle();
        home.clear_engagement_data_button.connect_clicked(move |_| {
            let Some(manager) = crate::feed::host::manager() else {
                crate::settings::render_error_label(
                    &error_label,
                    Some(strings::common::NOT_CONNECTED),
                );
                return;
            };
            let error_label = error_label.clone();
            crate::async_helper::spawn_with_snapshot(
                &runtime,
                move || async move { manager.delete_cue_rollup().await },
                move |result: Result<(), String>| {
                    crate::settings::render_error_label(&error_label, result.err().as_deref());
                },
            );
        });
    }

    // Layer-B signal-sharing pane: hydrate the opt-in + published list, flip on
    // toggle, refresh on page-visible — all over the shared FeedManager (the same
    // accessor the clear button uses).
    wire_signal_sharing(&home, fauna_client, &home_page);

    // ── Machine + observer wiring ───────────────────────────────────────
    let (tx, rx) = crate::async_helper::snapshot_wake_channel();
    let observer: Arc<dyn LabelerCatalogObserver> = Arc::new(GtkLabelerCatalogObserver { tx });
    let machine = build_machine(fauna_client, observer);

    {
        let machine = Arc::clone(&machine);
        let runtime = fauna_client.runtime_handle();
        catalog.inspect_close_button.connect_clicked(move |_| {
            machine.close_inspect();
            let _ = &runtime;
        });
    }

    // ── Render loop ─────────────────────────────────────────────────────
    let ctx = Rc::new(RenderCtx { home, catalog });
    {
        let machine = Arc::clone(&machine);
        let ctx = Rc::clone(&ctx);
        let runtime = fauna_client.runtime_handle();
        crate::async_helper::spawn_wake_loop(rx, move || {
            render_pages(&machine, &ctx, &runtime);
            glib::ControlFlow::Continue
        });
    }

    // Refresh the machine off WS-RPC whenever either sub-page becomes visible
    // (mirrors `devices_folders`'s `connect_map` refresh).
    for page in [&home_page, &catalog_page] {
        let machine = Arc::clone(&machine);
        let handle = fauna_client.runtime_handle();
        page.connect_map(move |_| {
            let machine = Arc::clone(&machine);
            handle.spawn(async move { machine.refresh().await });
        });
    }

    // Trained topics facet: sequenced account-store/PersonalizationClient calls
    // (no machine — the muted-words shape), refreshed on the same
    // visible-trigger so the registry + example counts stay current.
    let trained = trained_topics::wire(fauna_client, trained_widgets, ctx.home.error_label.clone());
    {
        let trained = Rc::new(trained);
        home_page.connect_map(move |_| trained.refresh());
    }

    let handles = PersonalizationHandles {
        labeler_catalog_machine: Arc::clone(&machine),
    };
    (home_page, catalog_page, handles)
}

/// Re-render both sub-pages off a fresh `LabelerCatalogSnapshot`.
fn render_pages(
    machine: &Arc<LabelerCatalogMachine>,
    ctx: &Rc<RenderCtx>,
    runtime: &tokio::runtime::Handle,
) {
    let snap = machine.snapshot();

    // Personalization home: only the caller's SUBSCRIBED labelers, indices
    // preserved into the full `entries` so gestures target the right row.
    let subscribed: Vec<(u32, &LabelerCatalogEntry)> = snap
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.subscribed)
        .map(|(i, e)| (i as u32, e))
        .collect();
    rebuild_rows(
        &ctx.home.labelers_list,
        machine,
        runtime,
        &subscribed,
        false,
    );
    // `personalization-labelers-empty` paints only once a refresh has
    // actually RETURNED — an empty `subscribed` list is indistinguishable
    // from "still loading" without `loaded` (`LabelerCatalogSnapshot::loaded`).
    ctx.home
        .labelers_empty
        .set_visible(snap.loaded && subscribed.is_empty());

    // Catalog page: every published labeler.
    let all: Vec<(u32, &LabelerCatalogEntry)> = snap
        .entries
        .iter()
        .enumerate()
        .map(|(i, e)| (i as u32, e))
        .collect();
    rebuild_rows(&ctx.catalog.catalog_list, machine, runtime, &all, true);
    // Same `loaded` gate as the home page's empty state above.
    ctx.catalog
        .catalog_empty
        .set_visible(snap.loaded && all.is_empty());

    render_inspect_panel(&ctx.catalog, snap.inspecting.as_ref());

    let text = snap.error.as_ref().map(|err| err.resolve(strings::lookup));
    crate::settings::render_error_label(&ctx.home.error_label, text.as_deref());
    crate::settings::render_error_label(&ctx.catalog.error_label, text.as_deref());
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::testid::widget_names;

    #[test]
    fn personalization_home_exposes_static_ui_yaml_ids() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let (shell, _trained) = build_home_shell();
            let names = widget_names(&shell.content);
            for id in [
                "page-heading",
                "error-message",
                "personalization-feeds-link",
                "personalization-muted-words-link",
                "personalization-labelers-list",
                "personalization-labelers-empty",
                "personalization-browse-catalog-button",
                "personalization-trained-factor-list",
                "personalization-trained-factor-create-button",
                "personalization-trained-factor-name-input",
                "personalization-clear-engagement-data-button",
                "personalization-share-signals-toggle",
                "signal-share-published-list",
                // The publish review-prune sheet's static family (the per-row
                // publish-button and the per-exemplar rows are indexed, built per
                // render, so they are not static-tree IDs).
                "personalization-trained-factor-publish-sheet",
                "personalization-trained-factor-publish-name-input",
                "personalization-trained-factor-publish-limitation-note",
                "personalization-trained-factor-publish-exemplar-list",
                "personalization-trained-factor-publish-exemplar-empty",
                "personalization-trained-factor-publish-submit-button",
                "personalization-trained-factor-publish-cancel-button",
            ] {
                assert!(
                    names.iter().any(|n| n == id),
                    "missing widget id {id:?}; have {names:?}"
                );
            }
        });
    }

    #[test]
    fn labeler_catalog_page_exposes_static_ui_yaml_ids() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let shell = build_catalog_shell();
            let names = widget_names(&shell.content);
            for id in [
                "page-heading",
                "error-message",
                "labeler-catalog-list",
                "labeler-catalog-empty",
                "labeler-inspect-panel",
                "labeler-inspect-metadata",
                "labeler-inspect-close-button",
            ] {
                assert!(
                    names.iter().any(|n| n == id),
                    "missing widget id {id:?}; have {names:?}"
                );
            }
        });
    }
}
