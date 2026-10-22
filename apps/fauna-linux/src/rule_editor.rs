//! The shared `rule-editor` component's paint on linux — ONE widget for every
//! mounting (`nest-rules.md` § The shared rule editor → *Three mountings, one
//! shape*): the admin Nest page hosts it in `admin-nest-rules-section`
//! (`views/admin.rs`), the Family page in `family-policy-rules-section` for
//! the selected ward (`views/family.rs`). tui's `rule_editor.rs` is the
//! reference leg.
//!
//! **This module paints; it decides nothing.** The rule list, the kinds, the
//! categories, the verdicts, the level scale, every label and defect sentence,
//! and whether the document may be saved are `fauna_client_policy::RuleEditor`'s
//! (its `RuleEditorView`), so the editor cannot differ between hosts or across
//! the 7 apps. A host owns a [`RuleEditorPanel`], loads it from its read, and
//! binds its save — which is where the write kind the offline gate reads is
//! decided.
//!
//! GTK keeps the widgets between edits, so a structural edit (add, remove, a
//! new kind swapping the row's subject control) rebuilds the rows, while a
//! typed input or a picker that changes no control only refreshes the save
//! button and the status line — typing never loses its entry's focus.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use fauna_client_policy::{LabelerChoice, RuleEditor, RuleKind, keywords_inline};
use fauna_core::content_policy::{ContentPolicyDocument, REASON_DEFAULT_KEY};
use fauna_core::localized::LocalizedText;
use fauna_ui_ids as ids;
use gtk::glib;

use crate::i18n::strings::rule_editor as t;

fn resolve(text: &LocalizedText) -> String {
    text.resolve(crate::i18n::strings::lookup)
}

/// A subscribed labeler the publisher picked: the host fetches its module
/// (`LabelersClient::rule_module`, [`fetch_labeler`]) and hands the bytes back
/// through [`RuleEditorPanel::labeler_fetched`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelerFetch {
    pub row: usize,
    /// The scorer name the rule will bundle it under.
    pub name: String,
    /// The labeler's hex id.
    pub labeler_id: String,
}

#[derive(Default)]
struct State {
    /// `None` until the host's read lands.
    editor: Option<RuleEditor>,
    /// The publisher's subscribed labelers a rule may name, as hex ids
    /// (`LabelersClient::rule_labelers`), read beside the document.
    labelers: Vec<String>,
    /// `rule-editor-status`'s text — the host's last verdict, or `None`.
    status: Option<String>,
}

type Callback = RefCell<Option<Rc<dyn Fn(&RuleEditorPanel)>>>;
type FetchCallback = RefCell<Option<Rc<dyn Fn(&RuleEditorPanel, LabelerFetch)>>>;

struct Inner {
    root: gtk::Box,
    rows: gtk::Box,
    empty: gtk::Label,
    add: gtk::Button,
    save: gtk::Button,
    status: gtk::Label,
    state: RefCell<State>,
    on_save: Callback,
    on_fetch: FetchCallback,
}

/// One mounting's editor widget and its working document. Cheap to clone; a
/// host's callbacks are handed the panel, so none needs to capture it.
#[derive(Clone)]
pub struct RuleEditorPanel {
    inner: Rc<Inner>,
}

impl RuleEditorPanel {
    /// Build an unloaded panel. `save_kind` is the wire kind the host's save
    /// issues — the offline gate's declaration (`crate::offline_gate`).
    pub fn new(save_kind: &'static str) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
        root.set_margin_start(12);
        root.set_margin_end(12);
        let empty = gtk::Label::new(Some(t::NO_RULES));
        empty.add_css_class("dim-label");
        empty.set_halign(gtk::Align::Start);
        root.append(&empty);
        let rows = gtk::Box::new(gtk::Orientation::Vertical, 8);
        root.append(&rows);
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let add = gtk::Button::with_label(t::ADD_BUTTON);
        crate::testid::set_test_id(&add, ids::RULE_EDITOR_ADD_BUTTON);
        buttons.append(&add);
        let save = gtk::Button::with_label(t::SAVE_BUTTON);
        save.add_css_class("suggested-action");
        crate::testid::set_test_id(&save, ids::RULE_EDITOR_SAVE_BUTTON);
        crate::offline_gate::declare_wire_kind(&save, save_kind);
        buttons.append(&save);
        root.append(&buttons);
        let status = gtk::Label::new(None);
        status.set_halign(gtk::Align::Start);
        status.set_xalign(0.0);
        status.set_wrap(true);
        crate::testid::set_test_id(&status, ids::RULE_EDITOR_STATUS);
        root.append(&status);

        let panel = Self {
            inner: Rc::new(Inner {
                root,
                rows,
                empty,
                add: add.clone(),
                save: save.clone(),
                status,
                state: RefCell::new(State::default()),
                on_save: RefCell::new(None),
                on_fetch: RefCell::new(None),
            }),
        };
        {
            let weak = Rc::downgrade(&panel.inner);
            add.connect_clicked(move |_| {
                let Some(inner) = weak.upgrade() else { return };
                if let Some(editor) = inner.state.borrow_mut().editor.as_mut() {
                    editor.add_rule(RuleKind::Category);
                }
                inner.state.borrow_mut().status = None;
                rebuild(&inner);
            });
        }
        {
            let weak = Rc::downgrade(&panel.inner);
            save.connect_clicked(move |_| {
                let Some(inner) = weak.upgrade() else { return };
                let cb = inner.on_save.borrow().clone();
                if let Some(cb) = cb {
                    cb(&RuleEditorPanel { inner });
                }
            });
        }
        rebuild(&panel.inner);
        panel
    }

    /// The component's root widget, appended by the host after its section
    /// landmark.
    pub fn widget(&self) -> &gtk::Box {
        &self.inner.root
    }

    /// Bind the host's save — run on `rule-editor-save-button`.
    pub fn connect_save(&self, f: impl Fn(&RuleEditorPanel) + 'static) {
        *self.inner.on_save.borrow_mut() = Some(Rc::new(f));
    }

    /// Bind the host's labeler fetch — run when a subscribed labeler the
    /// document does not bundle yet is picked.
    pub fn connect_labeler_fetch(&self, f: impl Fn(&RuleEditorPanel, LabelerFetch) + 'static) {
        *self.inner.on_fetch.borrow_mut() = Some(Rc::new(f));
    }

    /// Replace the working document — a fresh read or a landed save.
    pub fn load(&self, editor: RuleEditor) {
        self.inner.state.borrow_mut().editor = Some(editor);
        rebuild(&self.inner);
    }

    /// Back to the unloaded state — a new read is in flight (the selected
    /// ward changed, the page re-arrived).
    pub fn unload(&self) {
        {
            let mut s = self.inner.state.borrow_mut();
            s.editor = None;
            s.status = None;
        }
        rebuild(&self.inner);
    }

    /// The publisher's subscribed labelers, read beside the document.
    pub fn set_labelers(&self, labelers: Vec<String>) {
        self.inner.state.borrow_mut().labelers = labelers;
        rebuild(&self.inner);
    }

    /// The host's verdict on `rule-editor-status`; `None` returns the line to
    /// the crate's first defect.
    pub fn set_status(&self, status: Option<String>) {
        self.inner.state.borrow_mut().status = status;
        refresh_footer(&self.inner);
    }

    /// The working document, when the crate says it may be saved — the
    /// guard that keeps an agent click on a disabled button inert too.
    pub fn saveable_document(&self) -> Option<ContentPolicyDocument> {
        self.inner
            .state
            .borrow()
            .editor
            .as_ref()
            .filter(|e| e.can_save())
            .map(RuleEditor::document)
    }

    /// The loaded editor's publisher — a landed save re-seeds under it.
    pub fn publisher(&self) -> Option<fauna_core::content_policy::Publisher> {
        self.inner
            .state
            .borrow()
            .editor
            .as_ref()
            .map(|e| e.publisher().clone())
    }

    /// A picked labeler's module arrived: bundle it under the choice's name.
    /// `None` (not a usable module, or the read failed) leaves the rule
    /// without a labeler, whose defect the status line names.
    pub fn labeler_fetched(&self, fetch: &LabelerFetch, module: Option<Vec<u8>>) {
        if let Some(module) = module {
            let mut s = self.inner.state.borrow_mut();
            if let Some(editor) = s.editor.as_mut() {
                editor.set_labeler(fetch.row, &fetch.name, module);
                s.status = None;
            }
        }
        rebuild(&self.inner);
    }
}

/// Fetch a picked labeler's module for a rule to bundle — both hosts' network
/// half of a [`LabelerFetch`]. `None` when it is not a usable module or the
/// read failed (offline included); the rule then shows its missing-labeler
/// defect.
pub async fn fetch_labeler(
    nest: std::sync::Arc<fauna_client::NestClient>,
    labeler_id: String,
) -> Option<Vec<u8>> {
    let id = fauna_core::hex32::decode(&labeler_id).ok()?;
    fauna_client_labelers::LabelersClient::new(nest)
        .rule_module(id.to_vec())
        .await
        .ok()
        .flatten()
}

/// The publisher's subscribed labelers a rule may name — read beside each
/// mounting's document. A failed read lists none; the picker still offers
/// the document's own labelers.
pub async fn read_labelers(nest: std::sync::Arc<fauna_client::NestClient>) -> Vec<String> {
    fauna_client_labelers::LabelersClient::new(nest)
        .rule_labelers()
        .await
        .unwrap_or_default()
}

/// Bind both hosts' labeler fetch: a picked subscribed labeler's module is
/// read on the runtime and folded back into the panel.
pub fn bind_labeler_fetch(panel: &RuleEditorPanel, client: &Rc<crate::client::FaunaClient>) {
    let client = Rc::clone(client);
    panel.connect_labeler_fetch(move |panel, fetch| {
        let nest = std::sync::Arc::clone(client.nest_rpc());
        let labeler_id = fetch.labeler_id.clone();
        let panel = panel.clone();
        crate::async_helper::spawn_with_snapshot(
            &client.runtime_handle(),
            move || fetch_labeler(nest, labeler_id),
            move |module| panel.labeler_fetched(&fetch, module),
        );
    });
}

/// Rebuild after the current signal handler returns — a picker's own notify
/// must not destroy the picker mid-emission.
fn rebuild_later(inner: &Rc<Inner>) {
    let weak = Rc::downgrade(inner);
    glib::idle_add_local_once(move || {
        if let Some(inner) = weak.upgrade() {
            rebuild(&inner);
        }
    });
}

/// The save button and the status line: the host's verdict wins; before one,
/// the first thing that stops a save.
fn refresh_footer(inner: &Inner) {
    let s = inner.state.borrow();
    let Some(editor) = s.editor.as_ref() else {
        inner.save.set_sensitive(false);
        inner
            .status
            .set_text(s.status.as_deref().unwrap_or(t::LOADING));
        inner.status.set_visible(true);
        return;
    };
    let view = editor.view();
    inner.save.set_sensitive(view.can_save);
    match s
        .status
        .clone()
        .or_else(|| view.defects.first().map(resolve))
    {
        Some(text) => {
            inner.status.set_text(&text);
            inner.status.set_visible(true);
        }
        None => {
            inner.status.set_text("");
            inner.status.set_visible(false);
        }
    }
}

fn dropdown(options: &[String], selected: Option<usize>, live: bool) -> gtk::DropDown {
    let refs: Vec<&str> = options.iter().map(String::as_str).collect();
    let drop = gtk::DropDown::from_strings(&refs);
    drop.set_selected(
        selected
            .and_then(|i| u32::try_from(i).ok())
            .unwrap_or(gtk::INVALID_LIST_POSITION),
    );
    drop.set_sensitive(live);
    drop
}

fn field(label: &str, control: &impl IsA<gtk::Widget>) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let caption = gtk::Label::new(Some(label));
    caption.set_halign(gtk::Align::Start);
    caption.set_width_chars(12);
    caption.set_xalign(0.0);
    row.append(&caption);
    control.set_hexpand(true);
    row.append(control);
    row
}

/// Run `edit` on the working editor with the status cleared, then either
/// rebuild (`structural`) or refresh the footer.
fn edit(inner: &Rc<Inner>, structural: bool, f: impl FnOnce(&mut RuleEditor)) {
    {
        let mut s = inner.state.borrow_mut();
        let Some(editor) = s.editor.as_mut() else {
            return;
        };
        f(editor);
        s.status = None;
    }
    if structural {
        rebuild_later(inner);
    } else {
        refresh_footer(inner);
    }
}

/// Repaint every rule row from the crate's view.
fn rebuild(inner: &Rc<Inner>) {
    while let Some(child) = inner.rows.first_child() {
        inner.rows.remove(&child);
    }
    let (view, labeler_names) = {
        let s = inner.state.borrow();
        match s.editor.as_ref() {
            Some(editor) => (
                Some(editor.view()),
                editor
                    .labeler_choices(&s.labelers)
                    .into_iter()
                    .map(|c| c.name)
                    .collect::<Vec<_>>(),
            ),
            None => (None, Vec::new()),
        }
    };
    let Some(view) = view else {
        // A mounting whose read has not landed paints only its status.
        inner.empty.set_visible(false);
        inner.add.set_visible(false);
        inner.save.set_visible(false);
        refresh_footer(inner);
        return;
    };
    inner.add.set_visible(true);
    inner.save.set_visible(true);
    inner.add.set_sensitive(view.editable);
    inner.empty.set_visible(view.rows.is_empty());

    let kind_labels: Vec<String> = view.kinds.iter().map(|k| resolve(&k.label)).collect();
    let category_labels: Vec<String> = view.categories.iter().map(|c| resolve(&c.label)).collect();
    let verdict_labels: Vec<String> = view.verdicts.iter().map(|v| resolve(&v.label)).collect();
    let level_labels: Vec<String> = view.levels.iter().map(|l| resolve(&l.label)).collect();

    for (i, row) in view.rows.iter().enumerate() {
        let live = view.editable && !row.read_only;
        let item = gtk::Box::new(gtk::Orientation::Vertical, 4);
        item.add_css_class("card");
        item.set_margin_top(4);
        crate::testid::set_test_id(&item, ids::RULE_EDITOR_ITEM);
        let title = gtk::Label::new(Some(&t::item_label(&row.number.to_string())));
        title.add_css_class("heading");
        title.set_halign(gtk::Align::Start);
        item.append(&title);

        // Kind.
        let kind = dropdown(
            &kind_labels,
            view.kinds.iter().position(|k| k.kind == row.kind),
            live,
        );
        crate::testid::set_test_id(&kind, ids::RULE_EDITOR_ITEM_KIND_SELECT);
        {
            let weak = Rc::downgrade(inner);
            let kinds: Vec<RuleKind> = view.kinds.iter().map(|k| k.kind).collect();
            kind.connect_selected_notify(move |d| {
                let (Some(inner), Some(k)) = (weak.upgrade(), kinds.get(d.selected() as usize))
                else {
                    return;
                };
                let k = *k;
                edit(&inner, true, |e| {
                    e.set_kind(i, k);
                });
            });
        }
        item.append(&field(t::KIND_LABEL, &kind));

        // The one subject control, by kind.
        match row.kind {
            RuleKind::Category => {
                let selected = row
                    .category
                    .as_deref()
                    .and_then(|c| view.categories.iter().position(|o| o.category == c));
                let category = dropdown(&category_labels, selected, live);
                crate::testid::set_test_id(&category, ids::RULE_EDITOR_ITEM_CATEGORY_SELECT);
                let weak = Rc::downgrade(inner);
                let categories: Vec<String> =
                    view.categories.iter().map(|c| c.category.clone()).collect();
                category.connect_selected_notify(move |d| {
                    let (Some(inner), Some(c)) =
                        (weak.upgrade(), categories.get(d.selected() as usize))
                    else {
                        return;
                    };
                    let c = c.clone();
                    edit(&inner, false, |e| {
                        e.set_category(i, &c);
                    });
                });
                item.append(&field(t::CATEGORY_LABEL, &category));
            }
            RuleKind::Keywords => {
                let entry = gtk::Entry::new();
                entry.set_text(&keywords_inline(
                    row.keywords.as_deref().unwrap_or_default(),
                ));
                entry.set_sensitive(live);
                crate::testid::set_test_id(&entry, ids::RULE_EDITOR_ITEM_KEYWORDS_INPUT);
                let weak = Rc::downgrade(inner);
                entry.connect_changed(move |e| {
                    let Some(inner) = weak.upgrade() else { return };
                    let text = e.text().to_string();
                    edit(&inner, false, |ed| {
                        ed.set_keywords_inline(i, &text);
                    });
                });
                item.append(&field(t::KEYWORDS_LABEL, &entry));
            }
            RuleKind::Labeler => {
                let selected = row
                    .labeler
                    .as_deref()
                    .and_then(|l| labeler_names.iter().position(|n| n == l));
                let labeler = dropdown(&labeler_names, selected, live);
                crate::testid::set_test_id(&labeler, ids::RULE_EDITOR_ITEM_LABELER_SELECT);
                let weak = Rc::downgrade(inner);
                labeler.connect_selected_notify(move |d| {
                    let Some(inner) = weak.upgrade() else { return };
                    let index = d.selected() as usize;
                    let choice = {
                        let s = inner.state.borrow();
                        s.editor
                            .as_ref()
                            .and_then(|e| e.labeler_choices(&s.labelers).into_iter().nth(index))
                    };
                    match choice {
                        Some(LabelerChoice {
                            name,
                            labeler_id: None,
                        }) => edit(&inner, true, |e| {
                            e.choose_bundled_labeler(i, &name);
                        }),
                        Some(LabelerChoice {
                            name,
                            labeler_id: Some(labeler_id),
                        }) => {
                            let cb = inner.on_fetch.borrow().clone();
                            if let Some(cb) = cb {
                                let fetch = LabelerFetch {
                                    row: i,
                                    name,
                                    labeler_id,
                                };
                                cb(&RuleEditorPanel { inner }, fetch);
                            }
                        }
                        None => {}
                    }
                });
                item.append(&field(t::LABELER_LABEL, &labeler));
            }
            RuleKind::Prompt => {
                let entry = gtk::Entry::new();
                entry.set_text(row.prompt.as_deref().unwrap_or_default());
                entry.set_sensitive(live);
                crate::testid::set_test_id(&entry, ids::RULE_EDITOR_ITEM_PROMPT_INPUT);
                let weak = Rc::downgrade(inner);
                entry.connect_changed(move |e| {
                    let Some(inner) = weak.upgrade() else { return };
                    let text = e.text().to_string();
                    edit(&inner, false, |ed| {
                        ed.set_prompt(i, &text);
                    });
                });
                item.append(&field(t::PROMPT_LABEL, &entry));
            }
            // A loaded region list rule: shown, removable, never edited.
            RuleKind::List => {}
        }

        // Verdict.
        let verdict = dropdown(
            &verdict_labels,
            view.verdicts.iter().position(|v| v.verdict == row.verdict),
            live,
        );
        crate::testid::set_test_id(&verdict, ids::RULE_EDITOR_ITEM_VERDICT_SELECT);
        {
            let weak = Rc::downgrade(inner);
            let verdicts: Vec<_> = view.verdicts.iter().map(|v| v.verdict).collect();
            verdict.connect_selected_notify(move |d| {
                let (Some(inner), Some(v)) = (weak.upgrade(), verdicts.get(d.selected() as usize))
                else {
                    return;
                };
                let v = *v;
                edit(&inner, false, |e| {
                    e.set_verdict(i, v);
                });
            });
        }
        item.append(&field(t::VERDICT_LABEL, &verdict));

        // Level — absent on a keywords rule.
        if let Some(level_index) = row.level_index {
            let level = dropdown(&level_labels, Some(level_index as usize), live);
            crate::testid::set_test_id(&level, ids::RULE_EDITOR_ITEM_LEVEL_SELECT);
            let weak = Rc::downgrade(inner);
            level.connect_selected_notify(move |d| {
                let Some(inner) = weak.upgrade() else { return };
                let index = d.selected() as usize;
                edit(&inner, false, |e| {
                    e.set_level(i, index);
                });
            });
            item.append(&field(t::LEVEL_LABEL, &level));
        }

        // Reason, in the default language.
        let reason = gtk::Entry::new();
        reason.set_text(
            &row.reasons
                .iter()
                .find(|r| r.lang == REASON_DEFAULT_KEY)
                .map(|r| r.text.clone())
                .unwrap_or_default(),
        );
        reason.set_sensitive(live);
        crate::testid::set_test_id(&reason, ids::RULE_EDITOR_ITEM_REASON_INPUT);
        {
            let weak = Rc::downgrade(inner);
            reason.connect_changed(move |e| {
                let Some(inner) = weak.upgrade() else { return };
                let text = e.text().to_string();
                edit(&inner, false, |ed| {
                    ed.set_reason(i, REASON_DEFAULT_KEY, &text);
                });
            });
        }
        item.append(&field(t::REASON_LABEL, &reason));

        if let Some(inert) = &row.inert {
            let note = gtk::Label::new(Some(&resolve(inert)));
            note.add_css_class("dim-label");
            note.set_halign(gtk::Align::Start);
            item.append(&note);
        }

        let remove = gtk::Button::with_label(t::REMOVE_BUTTON);
        remove.set_halign(gtk::Align::Start);
        remove.set_sensitive(view.editable);
        crate::testid::set_test_id(&remove, ids::RULE_EDITOR_ITEM_REMOVE_BUTTON);
        {
            let weak = Rc::downgrade(inner);
            remove.connect_clicked(move |_| {
                let Some(inner) = weak.upgrade() else { return };
                edit(&inner, true, |e| {
                    e.remove_rule(i);
                });
            });
        }
        item.append(&remove);
        inner.rows.append(&item);
    }
    refresh_footer(inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::content_policy::Publisher;

    fn find(root: &gtk::Widget, id: &str) -> Vec<gtk::Widget> {
        let mut out = Vec::new();
        if root.widget_name() == id {
            out.push(root.clone());
        }
        let mut c = root.first_child();
        while let Some(child) = c {
            out.extend(find(&child, id));
            c = child.next_sibling();
        }
        out
    }

    /// The paint follows the crate: a new rule shows one subject control by
    /// kind, cannot be saved without a reason, and becomes saveable once its
    /// fields are typed.
    #[test]
    fn a_typed_keywords_rule_becomes_saveable() {
        crate::testid::run_on_gtk_thread(a_typed_keywords_rule_becomes_saveable_body);
    }

    fn a_typed_keywords_rule_becomes_saveable_body() {
        // The save is a declared write: connected, the page's own intent rules.
        crate::offline_gate::reset_for_test("connected");
        let p = RuleEditorPanel::new("fauna.admin.content_policy.set");
        p.load(RuleEditor::load(
            None,
            Publisher::Nest("example.test".to_string()),
        ));
        let root = p.widget().clone().upcast::<gtk::Widget>();
        assert!(find(&root, ids::RULE_EDITOR_ITEM).is_empty());
        p.inner.add.emit_clicked();
        assert_eq!(find(&root, ids::RULE_EDITOR_ITEM).len(), 1);
        assert_eq!(find(&root, ids::RULE_EDITOR_ITEM_CATEGORY_SELECT).len(), 1);
        assert!(!p.inner.save.is_sensitive(), "no reason yet");

        // Switch to keywords: the subject swaps and the level goes.
        p.inner
            .state
            .borrow_mut()
            .editor
            .as_mut()
            .unwrap()
            .set_kind(0, RuleKind::Keywords);
        rebuild(&p.inner);
        assert_eq!(find(&root, ids::RULE_EDITOR_ITEM_KEYWORDS_INPUT).len(), 1);
        assert!(find(&root, ids::RULE_EDITOR_ITEM_LEVEL_SELECT).is_empty());

        let keywords = find(&root, ids::RULE_EDITOR_ITEM_KEYWORDS_INPUT)[0]
            .clone()
            .downcast::<gtk::Entry>()
            .unwrap();
        keywords.set_text("casino, lottery");
        let reason = find(&root, ids::RULE_EDITOR_ITEM_REASON_INPUT)[0]
            .clone()
            .downcast::<gtk::Entry>()
            .unwrap();
        reason.set_text("No gambling here");
        assert!(p.inner.save.is_sensitive());
        let doc = p.saveable_document().expect("saveable");
        assert_eq!(doc.rules.len(), 1);
        assert_eq!(
            doc.scorers[0].keyword_terms().unwrap(),
            vec!["casino", "lottery"]
        );
    }
}
