//! The labeler inspect panel — `labeler-inspect-panel` and its metadata, list and
//! text-model sections (`ui/personalization.md`; ui.yaml `optional_elements`).
//!
//! One widget, two homes: the Community labelers catalog page paints it off its
//! own machine, and a room's labeler-set editor (`views/conversations/
//! room_settings_overlay.rs`) opens it in place so the staged draft survives
//! (`ui/conversations.md` § Element IDs → *The room's labeler set*). Nothing
//! here decides anything: it paints a `LabelerInspectView` the shared machine
//! produced.

use fauna_labeler_catalog_machine::LabelerInspectView;
use fauna_ui_ids as ids;
use gtk::prelude::*;

use crate::i18n::strings::labeler_catalog;
use crate::testid::set_test_id;

pub(crate) struct InspectPanel {
    pub root: gtk::Box,
    metadata: gtk::Label,
    /// The list-kind inspect section (hidden for `wasm` labelers): the decoded
    /// publisher-chosen name, the entry count, and the exact id→score rows —
    /// the frame's "renders the exact id→score map before subscribing".
    list_name: gtk::Label,
    list_entry_count: gtk::Label,
    list_entries: gtk::ListBox,
    /// The text-model-kind inspect section (hidden for every other kind): the
    /// decoded publisher-chosen name, the n-gram count, and the FULL
    /// vocabulary — the model's twin of the list section above.
    model_name: gtk::Label,
    model_ngram_count: gtk::Label,
    model_entries: gtk::ListBox,
    pub close_button: gtk::Button,
}

impl InspectPanel {
    pub fn new() -> Self {
        let inspect_panel = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .visible(false)
            .build();
        set_test_id(&inspect_panel, ids::LABELER_INSPECT_PANEL);
        let metadata = gtk::Label::builder()
            .wrap(true)
            .halign(gtk::Align::Start)
            .build();
        set_test_id(&metadata, ids::LABELER_INSPECT_METADATA);
        inspect_panel.append(&metadata);

        // List-kind section (ui.yaml optional_elements — visible only when the
        // inspected labeler is a `list` artifact).
        let list_name = gtk::Label::builder()
            .wrap(true)
            .halign(gtk::Align::Start)
            .visible(false)
            .build();
        set_test_id(&list_name, ids::LABELER_INSPECT_LIST_NAME);
        inspect_panel.append(&list_name);
        let list_entry_count = gtk::Label::builder()
            .halign(gtk::Align::Start)
            .visible(false)
            .build();
        list_entry_count.add_css_class("dim-label");
        set_test_id(&list_entry_count, ids::LABELER_INSPECT_LIST_ENTRY_COUNT);
        inspect_panel.append(&list_entry_count);
        let list_entries = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .visible(false)
            .build();
        set_test_id(&list_entries, ids::LABELER_INSPECT_LIST_ENTRIES);
        inspect_panel.append(&list_entries);

        // Text-model-kind section (ui.yaml optional_elements — visible only when
        // the inspected labeler is a `text-model` artifact), the list section's
        // twin.
        let model_name = gtk::Label::builder()
            .wrap(true)
            .halign(gtk::Align::Start)
            .visible(false)
            .build();
        set_test_id(&model_name, ids::LABELER_INSPECT_MODEL_NAME);
        inspect_panel.append(&model_name);
        let model_ngram_count = gtk::Label::builder()
            .halign(gtk::Align::Start)
            .visible(false)
            .build();
        model_ngram_count.add_css_class("dim-label");
        set_test_id(&model_ngram_count, ids::LABELER_INSPECT_MODEL_NGRAM_COUNT);
        inspect_panel.append(&model_ngram_count);
        let model_entries = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .visible(false)
            .build();
        set_test_id(&model_entries, ids::LABELER_INSPECT_MODEL_ENTRIES);
        inspect_panel.append(&model_entries);

        let close_button = gtk::Button::with_label(labeler_catalog::CLOSE_INSPECT);
        close_button.set_halign(gtk::Align::Start);
        set_test_id(&close_button, ids::LABELER_INSPECT_CLOSE_BUTTON);
        inspect_panel.append(&close_button);
        Self {
            root: inspect_panel,
            metadata,
            list_name,
            list_entry_count,
            list_entries,
            model_name,
            model_ngram_count,
            model_entries,
            close_button,
        }
    }

    /// Render the inspect panel from the snapshot's `inspecting` view, if any.
    /// For a `list` artifact the panel additionally renders the decoded
    /// publisher-chosen name + the **exact** id→score map (frame § Tier-3 artifact
    /// kinds — every entry, never a capped preview: a truncated map is not the
    /// exact map).
    pub fn render(&self, inspecting: Option<&LabelerInspectView>) {
        let shell = self;
        while let Some(child) = shell.list_entries.first_child() {
            shell.list_entries.remove(&child);
        }
        while let Some(child) = shell.model_entries.first_child() {
            shell.model_entries.remove(&child);
        }
        match inspecting {
            Some(view) => {
                let text = format!(
                    "labeler_id: {}\nversion: {}\nartifact_kind: {}\nwasm_hash: {}\nwasm_size: {}\nneeds_text: {}\nneeds_hashtags: {}\nneeds_media_metadata: {}\nneeds_author: {}\nneeds_attachment_bytes: {}\nlabel_abi: {}\nverified: {}{}",
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
                    view.label_abi,
                    view.verified,
                    // A revision past this app's own reads inert and says so
                    // (content-moderation-and-ranking.md § Tier-3 → *The output
                    // half of the `label()` ABI*, position (ii)).
                    if view.newer_than_this_app {
                        format!("\n{}", labeler_catalog::INSPECT_NEEDS_NEWER_APP)
                    } else {
                        String::new()
                    },
                );
                shell.metadata.set_text(&text);

                let is_list = view.artifact_kind == "list";
                shell.list_name.set_visible(is_list);
                shell.list_entry_count.set_visible(is_list);
                shell.list_entries.set_visible(is_list);
                if is_list {
                    // An unnamed list is valid (pre-name artifacts) — say so rather
                    // than rendering an empty label.
                    shell.list_name.set_text(&match &view.list_name {
                        Some(name) => labeler_catalog::list_name(name),
                        None => labeler_catalog::UNNAMED_LIST.to_string(),
                    });
                    shell
                        .list_entry_count
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

                        shell.list_entries.append(&row);
                    }
                }

                // The text-model section, the same optional_elements shape: it
                // registers only for a `text-model` artifact, keyed on the KIND
                // rather than on the payload being non-empty — an empty
                // vocabulary is a decode failure, not a reason to render the
                // panel as if it were some other kind.
                let is_model = view.artifact_kind == fauna_core::scoring::artifact_kind::TEXT_MODEL;
                shell.model_name.set_visible(is_model);
                shell.model_ngram_count.set_visible(is_model);
                shell.model_entries.set_visible(is_model);
                if is_model {
                    shell.model_name.set_text(&match &view.model_name {
                        Some(name) => labeler_catalog::model_name(name),
                        None => labeler_catalog::UNNAMED_MODEL.to_string(),
                    });
                    shell
                        .model_ngram_count
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

                        shell.model_entries.append(&row);
                    }
                }
                shell.root.set_visible(true);
            }
            None => shell.root.set_visible(false),
        }
    }
}
