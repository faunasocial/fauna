//! The shared `rule-editor` component's paint — ONE render for every mounting
//! (`nest-rules.md` § The shared rule editor → *Three mountings, one shape*):
//! the admin Nest page hosts it in `admin-nest-rules-section` (`crate::admin`),
//! the Family page in `family-policy-rules-section` for the selected ward.
//!
//! **This module paints; it decides nothing.** The rule list, the kinds, the
//! categories, the verdicts, the level scale, every label and defect sentence,
//! and whether the document may be saved are `fauna_client_policy::RuleEditor`'s
//! (its `RuleEditorView`), so the editor cannot differ between hosts or across
//! the 7 apps. A host owns a [`Mount`] (the working editor plus its status
//! line), routes the edits below back to it, and binds its own save — which is
//! where the write kind the offline gate reads is decided.

use std::collections::BTreeMap;

use fauna_client_policy::{LabelerChoice, RuleEditor, RuleKind, RuleVerdict, keywords_inline};
use fauna_core::localized::LocalizedText;
use fauna_i18n::strings::rule_editor as t;
use fauna_ui_ids as ids;

use crate::element::{Element, Field, Gesture, SelectTarget};

/// Which mounting a select belongs to — carried by
/// [`SelectTarget::RuleEditor`] so a picker routes back to its host's page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    /// `admin-nest-rules-section` — the nest's published rules.
    Admin,
    /// `family-policy-rules-section` — the selected ward's content rules.
    Family,
}

/// The per-rule pickers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Picker {
    Kind,
    Category,
    Labeler,
    Verdict,
    Level,
}

/// A local edit of the working document — nothing is written until the
/// host's save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    Add,
    Remove(usize),
    /// A picker's new value, as painted (its option label).
    Pick {
        row: usize,
        picker: Picker,
        value: String,
    },
}

/// A per-rule text input.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Input {
    Keywords(usize),
    Prompt(usize),
    Reason(usize),
}

/// A subscribed labeler the guardian or admin picked: the host fetches its
/// module (`LabelersClient::rule_module`) and hands the bytes back through
/// [`Mount::labeler_fetched`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelerFetch {
    pub row: usize,
    /// The scorer name the rule will bundle it under.
    pub name: String,
    /// The labeler's hex id.
    pub labeler_id: String,
}

/// One mounting's editor: the working document and the status line.
#[derive(Debug, Default)]
pub struct Mount {
    /// `None` until the host's read lands.
    pub editor: Option<RuleEditor>,
    /// The publisher's subscribed labelers a rule may name, as hex ids
    /// (`LabelersClient::rule_labelers`), read beside the document.
    pub labelers: Vec<String>,
    /// The keywords inputs as typed. A single-line input re-painted from the
    /// stored terms would eat a trailing comma mid-typing, so the typed text is
    /// kept per row while the editor holds the parsed terms.
    keywords: BTreeMap<usize, String>,
    /// `rule-editor-status`'s text — the host's last verdict, or `None`.
    pub status: Option<String>,
}

impl Mount {
    /// Replace the working document — a fresh read or a landed save.
    pub fn load(&mut self, editor: RuleEditor) {
        self.editor = Some(editor);
        self.keywords.clear();
    }

    /// Apply a local edit. Picking a subscribed labeler the document does not
    /// bundle yet returns the fetch the host runs; everything else is local.
    pub fn apply(&mut self, edit: Edit) -> Option<LabelerFetch> {
        let editor = self.editor.as_mut()?;
        let mut fetch = None;
        match edit {
            Edit::Add => {
                editor.add_rule(RuleKind::Category);
            }
            Edit::Remove(row) => {
                if editor.remove_rule(row) {
                    // Rows after `row` shift up; their typed text is re-read
                    // from the stored terms.
                    self.keywords.clear();
                }
            }
            Edit::Pick { row, picker, value } => {
                let view = editor.view();
                match picker {
                    Picker::Kind => {
                        if let Some(k) = view.kinds.iter().find(|k| resolve(&k.label) == value) {
                            editor.set_kind(row, k.kind);
                            self.keywords.remove(&row);
                        }
                    }
                    Picker::Category => {
                        if let Some(c) = view.categories.iter().find(|c| resolve(&c.label) == value)
                        {
                            editor.set_category(row, &c.category);
                        }
                    }
                    Picker::Labeler => {
                        match editor
                            .labeler_choices(&self.labelers)
                            .into_iter()
                            .find(|c| c.name == value)
                        {
                            Some(LabelerChoice {
                                name,
                                labeler_id: None,
                            }) => {
                                editor.choose_bundled_labeler(row, &name);
                            }
                            Some(LabelerChoice {
                                name,
                                labeler_id: Some(labeler_id),
                            }) => {
                                fetch = Some(LabelerFetch {
                                    row,
                                    name,
                                    labeler_id,
                                });
                            }
                            None => {}
                        }
                    }
                    Picker::Verdict => {
                        if let Some(v) = view.verdicts.iter().find(|v| resolve(&v.label) == value) {
                            editor.set_verdict(row, v.verdict);
                        }
                    }
                    Picker::Level => {
                        if let Some(i) = view.levels.iter().position(|l| resolve(&l.label) == value)
                        {
                            editor.set_level(row, i);
                        }
                    }
                }
            }
        }
        self.status = None;
        fetch
    }

    /// A picked labeler's module arrived: bundle it under the choice's name.
    /// `None` (not a usable module, or the read failed) leaves the rule
    /// without a labeler, whose defect the status line names.
    pub fn labeler_fetched(&mut self, fetch: &LabelerFetch, module: Option<Vec<u8>>) {
        if let (Some(editor), Some(module)) = (self.editor.as_mut(), module) {
            editor.set_labeler(fetch.row, &fetch.name, module);
            self.status = None;
        }
    }

    /// The text an input paints.
    pub fn field(&self, input: &Input) -> String {
        let Some(row) = self.editor.as_ref().and_then(|e| {
            let index = match input {
                Input::Keywords(i) | Input::Prompt(i) | Input::Reason(i) => *i,
            };
            e.view().rows.into_iter().nth(index)
        }) else {
            return String::new();
        };
        match input {
            Input::Keywords(i) => self
                .keywords
                .get(i)
                .cloned()
                .unwrap_or_else(|| keywords_inline(row.keywords.as_deref().unwrap_or_default())),
            Input::Prompt(_) => row.prompt.unwrap_or_default(),
            Input::Reason(_) => row
                .reasons
                .into_iter()
                .find(|r| r.lang == fauna_core::content_policy::REASON_DEFAULT_KEY)
                .map(|r| r.text)
                .unwrap_or_default(),
        }
    }

    /// Write an input.
    pub fn set_field(&mut self, input: Input, value: String) {
        let Some(editor) = self.editor.as_mut() else {
            return;
        };
        match input {
            Input::Keywords(i) => {
                editor.set_keywords_inline(i, &value);
                self.keywords.insert(i, value);
            }
            Input::Prompt(i) => {
                editor.set_prompt(i, &value);
            }
            Input::Reason(i) => {
                editor.set_reason(i, fauna_core::content_policy::REASON_DEFAULT_KEY, &value);
            }
        }
        self.status = None;
    }
}

/// Fetch a picked labeler's module for a rule to bundle — both hosts' network
/// half of a [`LabelerFetch`]. `None` when it is not a usable module or the
/// read failed (offline included); the rule then shows its missing-labeler
/// defect.
pub async fn fetch_labeler(
    nest: &std::sync::Arc<fauna_client::NestClient>,
    fetch: &LabelerFetch,
) -> Option<Vec<u8>> {
    let id = fauna_core::hex32::decode(&fetch.labeler_id).ok()?;
    fauna_client_labelers::LabelersClient::new(std::sync::Arc::clone(nest))
        .rule_module(id.to_vec())
        .await
        .ok()
        .flatten()
}

/// The publisher's subscribed labelers a rule may name — read beside each
/// mounting's document. A failed read lists none; the picker still offers
/// the document's own labelers.
pub async fn read_labelers(nest: &std::sync::Arc<fauna_client::NestClient>) -> Vec<String> {
    fauna_client_labelers::LabelersClient::new(std::sync::Arc::clone(nest))
        .rule_labelers()
        .await
        .unwrap_or_default()
}

fn resolve(text: &LocalizedText) -> String {
    text.resolve(fauna_i18n::strings::lookup)
}

/// The host's bindings: how an edit and an input reach its page, and its save.
pub struct Bindings {
    pub host: Host,
    pub edit: fn(Edit) -> Gesture,
    pub input: fn(Input) -> Field,
    pub save: Gesture,
}

/// The component's elements, in paint order, after the host's own section
/// landmark. A mounting whose read has not landed paints only its status.
pub fn elements(mount: &Mount, bindings: Bindings) -> Vec<Element> {
    let Some(editor) = mount.editor.as_ref() else {
        return vec![Element::label(
            ids::RULE_EDITOR_STATUS,
            mount
                .status
                .clone()
                .unwrap_or_else(|| t::LOADING.to_string()),
        )];
    };
    let view = editor.view();
    let editable = view.editable;
    let mut els = Vec::new();
    if view.rows.is_empty() {
        els.push(Element::chrome(t::NO_RULES));
    }
    let kind_labels: Vec<String> = view.kinds.iter().map(|k| resolve(&k.label)).collect();
    let verdict_labels: Vec<String> = view.verdicts.iter().map(|v| resolve(&v.label)).collect();
    let level_labels: Vec<String> = view.levels.iter().map(|l| resolve(&l.label)).collect();
    let category_labels: Vec<String> = view.categories.iter().map(|c| resolve(&c.label)).collect();
    let labeler_names: Vec<String> = editor
        .labeler_choices(&mount.labelers)
        .into_iter()
        .map(|c| c.name)
        .collect();
    let item = ids::RULE_EDITOR_ITEM;
    for (i, row) in view.rows.iter().enumerate() {
        let live = editable && !row.read_only;
        let select = |id: &str, picker: Picker, selected: String, options: Vec<String>| {
            Element::select(
                id,
                selected,
                SelectTarget::RuleEditor {
                    host: bindings.host,
                    row: i,
                    picker,
                },
                options,
            )
            .enabled(live)
            .within(item, i)
        };
        els.push(Element::label(item, t::item_label(&row.number.to_string())).within(item, i));
        els.push(
            select(
                ids::RULE_EDITOR_ITEM_KIND_SELECT,
                Picker::Kind,
                resolve(&row.kind_label),
                kind_labels.clone(),
            )
            .labelled(t::KIND_LABEL),
        );
        match row.kind {
            RuleKind::Category => {
                let category = row.category.clone().unwrap_or_default();
                let selected = view
                    .categories
                    .iter()
                    .find(|c| c.category == category)
                    .map(|c| resolve(&c.label))
                    .unwrap_or(category);
                els.push(
                    select(
                        ids::RULE_EDITOR_ITEM_CATEGORY_SELECT,
                        Picker::Category,
                        selected,
                        category_labels.clone(),
                    )
                    .labelled(t::CATEGORY_LABEL),
                );
            }
            RuleKind::Keywords => els.push(
                Element::input(
                    ids::RULE_EDITOR_ITEM_KEYWORDS_INPUT,
                    mount.field(&Input::Keywords(i)),
                    (bindings.input)(Input::Keywords(i)),
                )
                .labelled(t::KEYWORDS_LABEL)
                .enabled(live)
                .within(item, i),
            ),
            RuleKind::Labeler => els.push(
                select(
                    ids::RULE_EDITOR_ITEM_LABELER_SELECT,
                    Picker::Labeler,
                    row.labeler.clone().unwrap_or_default(),
                    labeler_names.clone(),
                )
                .labelled(t::LABELER_LABEL),
            ),
            RuleKind::Prompt => els.push(
                Element::input(
                    ids::RULE_EDITOR_ITEM_PROMPT_INPUT,
                    mount.field(&Input::Prompt(i)),
                    (bindings.input)(Input::Prompt(i)),
                )
                .labelled(t::PROMPT_LABEL)
                .enabled(live)
                .within(item, i),
            ),
            // A loaded region list rule: shown, removable, never edited.
            RuleKind::List => {}
        }
        let verdict = match row.verdict {
            RuleVerdict::Block => &view.verdicts[0],
            RuleVerdict::Collapse => &view.verdicts[1],
        };
        els.push(
            select(
                ids::RULE_EDITOR_ITEM_VERDICT_SELECT,
                Picker::Verdict,
                resolve(&verdict.label),
                verdict_labels.clone(),
            )
            .labelled(t::VERDICT_LABEL),
        );
        if let Some(level) = row.level_index.and_then(|l| view.levels.get(l as usize)) {
            els.push(
                select(
                    ids::RULE_EDITOR_ITEM_LEVEL_SELECT,
                    Picker::Level,
                    resolve(&level.label),
                    level_labels.clone(),
                )
                .labelled(t::LEVEL_LABEL),
            );
        }
        els.push(
            Element::input(
                ids::RULE_EDITOR_ITEM_REASON_INPUT,
                mount.field(&Input::Reason(i)),
                (bindings.input)(Input::Reason(i)),
            )
            .labelled(t::REASON_LABEL)
            .enabled(live)
            .within(item, i),
        );
        if let Some(inert) = &row.inert {
            els.push(Element::chrome(resolve(inert)));
        }
        els.push(
            Element::gesture_button(
                ids::RULE_EDITOR_ITEM_REMOVE_BUTTON,
                t::REMOVE_BUTTON,
                editable,
                (bindings.edit)(Edit::Remove(i)),
            )
            .within(item, i),
        );
    }
    els.push(Element::gesture_button(
        ids::RULE_EDITOR_ADD_BUTTON,
        t::ADD_BUTTON,
        editable,
        (bindings.edit)(Edit::Add),
    ));
    els.push(Element::gesture_button(
        ids::RULE_EDITOR_SAVE_BUTTON,
        t::SAVE_BUTTON,
        view.can_save,
        bindings.save,
    ));
    // The host's verdict wins; before one, the first thing that stops a save.
    let status = mount
        .status
        .clone()
        .or_else(|| view.defects.first().map(resolve));
    if let Some(status) = status {
        els.push(Element::label(ids::RULE_EDITOR_STATUS, status));
    }
    els
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::content_policy::Publisher;

    fn bindings() -> Bindings {
        Bindings {
            host: Host::Admin,
            edit: |e| Gesture::Admin(crate::admin::Action::RuleEditor(e)),
            input: |i| Field::Admin(crate::admin::AdminField::RuleEditor(i)),
            save: Gesture::Admin(crate::admin::Action::SaveNestRules),
        }
    }

    fn mount() -> Mount {
        let mut m = Mount::default();
        m.load(RuleEditor::load(
            None,
            Publisher::Nest("example.test".to_string()),
        ));
        m
    }

    fn ids_of(els: &[Element]) -> Vec<&str> {
        els.iter()
            .map(|e| e.id.as_str())
            .filter(|id| !id.is_empty())
            .collect()
    }

    fn text_of<'a>(els: &'a [Element], id: &str) -> Option<&'a str> {
        els.iter().find(|e| e.id == id).map(|e| e.text.as_str())
    }

    #[test]
    fn an_unloaded_mount_paints_only_its_status() {
        let els = elements(&Mount::default(), bindings());
        assert_eq!(ids_of(&els), vec![ids::RULE_EDITOR_STATUS]);
        assert_eq!(text_of(&els, ids::RULE_EDITOR_STATUS), Some(t::LOADING));
    }

    #[test]
    fn a_new_rule_paints_one_subject_control_by_kind_and_cannot_save_without_a_reason() {
        let mut m = mount();
        m.apply(Edit::Add);
        let els = elements(&m, bindings());
        let ids = ids_of(&els);
        assert!(ids.contains(&ids::RULE_EDITOR_ITEM_CATEGORY_SELECT));
        assert!(!ids.contains(&ids::RULE_EDITOR_ITEM_KEYWORDS_INPUT));
        assert!(ids.contains(&ids::RULE_EDITOR_ITEM_LEVEL_SELECT));
        let save = els
            .iter()
            .find(|e| e.id == ids::RULE_EDITOR_SAVE_BUTTON)
            .unwrap();
        assert!(!save.enabled, "a rule without a reason cannot be saved");
        // The crate's defect sentence is the status before any verdict.
        assert_eq!(
            text_of(&els, ids::RULE_EDITOR_STATUS),
            Some("Rule 1 needs a reason.")
        );

        // Switching the kind swaps the subject control; a keywords rule has
        // no level.
        m.apply(Edit::Pick {
            row: 0,
            picker: Picker::Kind,
            value: "Keywords".to_string(),
        });
        let ids_after = ids_of(&elements(&m, bindings()))
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        assert!(
            ids_after
                .iter()
                .any(|i| i == ids::RULE_EDITOR_ITEM_KEYWORDS_INPUT)
        );
        assert!(
            !ids_after
                .iter()
                .any(|i| i == ids::RULE_EDITOR_ITEM_CATEGORY_SELECT)
        );
        assert!(
            !ids_after
                .iter()
                .any(|i| i == ids::RULE_EDITOR_ITEM_LEVEL_SELECT)
        );
    }

    #[test]
    fn typed_keywords_and_reason_make_the_document_saveable() {
        let mut m = mount();
        m.apply(Edit::Add);
        m.apply(Edit::Pick {
            row: 0,
            picker: Picker::Kind,
            value: "Keywords".to_string(),
        });
        // A trailing comma mid-typing survives the repaint.
        m.set_field(Input::Keywords(0), "casino,".to_string());
        assert_eq!(m.field(&Input::Keywords(0)), "casino,");
        m.set_field(Input::Keywords(0), "casino, lottery".to_string());
        m.set_field(Input::Reason(0), "No gambling here".to_string());
        m.apply(Edit::Pick {
            row: 0,
            picker: Picker::Verdict,
            value: "Hide behind a warning".to_string(),
        });
        let els = elements(&m, bindings());
        let save = els
            .iter()
            .find(|e| e.id == ids::RULE_EDITOR_SAVE_BUTTON)
            .unwrap();
        assert!(save.enabled);
        assert_eq!(text_of(&els, ids::RULE_EDITOR_STATUS), None);
        let doc = m.editor.as_ref().unwrap().document();
        assert_eq!(doc.rules.len(), 1);
        assert_eq!(
            doc.rules[0].verdict,
            fauna_core::content_policy::ContentVerdict::Collapse
        );
        assert_eq!(
            doc.scorers[0].keyword_terms().unwrap(),
            vec!["casino", "lottery"]
        );

        m.apply(Edit::Remove(0));
        assert!(m.editor.as_ref().unwrap().document().rules.is_empty());
    }

    /// The labeler picker lists the publisher's subscriptions; picking one the
    /// document does not bundle asks the host for its module, and the arrived
    /// module becomes the rule's labeler — offered as bundled from then on.
    #[test]
    fn picking_a_subscribed_labeler_fetches_its_module_and_bundles_it() {
        let id = "ab".repeat(32);
        let name = fauna_client_policy::labeler_scorer_name(&id);
        let mut m = mount();
        m.labelers = vec![id.clone()];
        m.apply(Edit::Add);
        m.apply(Edit::Pick {
            row: 0,
            picker: Picker::Kind,
            value: "Labeler".to_string(),
        });
        let els = elements(&m, bindings());
        let picker = els
            .iter()
            .find(|e| e.id == ids::RULE_EDITOR_ITEM_LABELER_SELECT)
            .expect("a labeler rule paints its picker");
        assert!(picker.enabled, "the picker is live");
        match &picker.role {
            crate::element::Role::Select { options, .. } => {
                assert_eq!(options, std::slice::from_ref(&name))
            }
            other => panic!("expected a select, got {other:?}"),
        }

        let fetch = m
            .apply(Edit::Pick {
                row: 0,
                picker: Picker::Labeler,
                value: name.clone(),
            })
            .expect("a subscribed labeler is fetched");
        assert_eq!(fetch.labeler_id, id);
        assert_eq!(fetch.name, name);
        m.labeler_fetched(&fetch, Some(b"module".to_vec()));
        let view = m.editor.as_ref().unwrap().view();
        assert_eq!(view.rows[0].labeler.as_deref(), Some(name.as_str()));

        // A second labeler rule shares the bundled module with no fetch.
        m.apply(Edit::Add);
        m.apply(Edit::Pick {
            row: 1,
            picker: Picker::Kind,
            value: "Labeler".to_string(),
        });
        assert_eq!(
            m.apply(Edit::Pick {
                row: 1,
                picker: Picker::Labeler,
                value: name.clone(),
            }),
            None
        );
        let doc = m.editor.as_ref().unwrap().document();
        assert_eq!(doc.rules[0].factor, doc.rules[1].factor);
    }
}
