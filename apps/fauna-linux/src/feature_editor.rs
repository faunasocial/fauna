//! The shared `feature-policy-editor` family's paint — ONE render for both of
//! its hosts (`dynamic-features.md` § Authoring surfaces): the admin Nest page
//! opens it at the admin tier (`views::admin`), a Feature limits row on the
//! Status sub-page at the self tier (`views::status`). The twin of tui's
//! `feature_editor.rs`.
//!
//! **This module paints; it decides nothing.** The draft, the cell list and its
//! order, the labels, the no-effect notes, whether *Remove limit* renders and
//! the parser behind *Save* are all `fauna_client_features::PolicyEditor`'s, so
//! the editor cannot differ between the two hosts or across the 7 apps. The
//! hosts differ only in the tier they open it at — which decides the write kind
//! the offline gate reads and the call [`crate::client::FaunaClient`] makes.
//!
//! State lives in per-host thread-locals (GTK is single-threaded): the authored
//! read, the open editor and the last verdict line. Every handler mutates the
//! state in a short borrow and then asks the host to repaint, so a repaint never
//! runs under a live borrow.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use fauna_client_features::{AuthoredSurface, AuthoringTier, PolicyEditor, editor_note_text};
use fauna_ui_ids as ids;
use gtk::glib;

use crate::client::FaunaClient;
use crate::i18n::strings::features as f;

/// Which page hosts the editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    /// The admin Nest page's *Feature limits for everyone* section.
    Admin,
    /// The Status sub-page's per-row *Your own limit* control.
    SelfTier,
    /// The Family page's *Feature limits* section for the SELECTED ward
    /// (`family-safety.md` § App surface → *Feature limits*). Unlike the other
    /// two, its tier carries the ward, which lives in the host's state.
    Guardian,
}

impl Host {
    /// The tier of a host whose tier is fixed; `None` for [`Host::Guardian`],
    /// whose tier is ward-keyed ([`HostState::ward`]).
    pub fn static_tier(self) -> Option<AuthoringTier> {
        match self {
            Host::Admin => Some(AuthoringTier::Admin),
            Host::SelfTier => Some(AuthoringTier::SelfImposed),
            Host::Guardian => None,
        }
    }

    /// The wire kind the host's Save and Remove end in — the offline gate's
    /// input. `write_kind` ignores the ward, so a placeholder names the guardian
    /// tier's kind.
    fn write_kind(self) -> &'static str {
        self.static_tier()
            .unwrap_or(AuthoringTier::Guardian { ward: [0; 32] })
            .write_kind()
    }
}

/// One host's state: the authored read, the open editor and its verdict line.
#[derive(Default)]
pub struct HostState {
    /// `None` until the authored read answers — a summary painted before that
    /// would say "No limit set" with no basis.
    pub surface: Option<AuthoredSurface>,
    /// The worded reason the authored read failed (admin host only paints it).
    pub read_failed: Option<String>,
    pub editor: Option<PolicyEditor>,
    /// The host's last verdict line (`feature-policy-editor-status`), present
    /// only after a write.
    pub status: Option<String>,
    /// The ward the guardian host is pointed at (`Host::Guardian` only).
    pub ward: Option<[u8; 32]>,
}

thread_local! {
    static ADMIN: RefCell<HostState> = RefCell::new(HostState::default());
    static OWN: RefCell<HostState> = RefCell::new(HostState::default());
    static GUARDIAN: RefCell<HostState> = RefCell::new(HostState::default());
    /// The signed-in client the Save / Remove buttons write through. Bound at
    /// login beside the first `fetch_features`.
    static CLIENT: RefCell<Option<Rc<FaunaClient>>> = const { RefCell::new(None) };
}

pub fn bind_client(client: &Rc<FaunaClient>) {
    CLIENT.with_borrow_mut(|c| *c = Some(Rc::clone(client)));
}

/// Run `f` over one host's state. Keep `f` short and never paint inside it.
pub fn with_state<R>(host: Host, f: impl FnOnce(&mut HostState) -> R) -> R {
    match host {
        Host::Admin => ADMIN.with_borrow_mut(f),
        Host::SelfTier => OWN.with_borrow_mut(f),
        Host::Guardian => GUARDIAN.with_borrow_mut(f),
    }
}

/// Point the guardian host at `ward` with the surface folded from its fresh
/// status entry. An editor is one ward's: it closes whenever the ward changes,
/// and is kept (same ward, e.g. after a save's re-read) otherwise. `surface` is
/// `None` until the nest's capability set has been read — no rows rather than
/// rows for a plane this nest build may not carry.
pub fn set_ward(ward: [u8; 32], surface: Option<AuthoredSurface>) {
    with_state(Host::Guardian, |s| {
        if s.ward != Some(ward) {
            s.editor = None;
            s.status = None;
        }
        s.ward = Some(ward);
        s.surface = surface;
    });
}

/// No ward is selected (the guardian has none): nothing to paint or edit.
pub fn clear_ward() {
    with_state(Host::Guardian, |s| *s = HostState::default());
}

/// An authored read landed (or failed). The editor, if open, is kept.
pub fn set_surface(host: Host, result: Result<AuthoredSurface, String>) {
    with_state(host, |s| match result {
        Ok(surface) => {
            s.surface = Some(surface);
            s.read_failed = None;
        }
        Err(reason) => {
            s.surface = None;
            s.read_failed = Some(reason);
        }
    });
}

/// A write landed: re-seed the open editor from the fresh authored read and
/// show the verdict line.
pub fn set_written(host: Host, status: String, editor: Option<PolicyEditor>) {
    with_state(host, |s| {
        s.editor = editor;
        s.status = Some(status);
    });
}

/// Open the editor for `feature` at host's tier, over the authored read.
pub fn open(host: Host, feature: &str) {
    with_state(host, |s| {
        let tier = match host.static_tier() {
            Some(tier) => Some(tier),
            None => s.ward.map(|ward| AuthoringTier::Guardian { ward }),
        };
        s.editor = tier.and_then(|tier| s.surface.as_ref()?.editor(tier, feature));
        s.status = None;
    });
}

fn cancel(host: Host) {
    with_state(host, |s| {
        s.editor = None;
        s.status = None;
    });
}

/// Build the open editor's widgets. `repaint` re-renders the host after a
/// change that alters the editor's SHAPE (the On/Off choice, Cancel); typing in
/// a cell updates its note in place so the entry keeps focus.
pub fn build_editor(host: Host, repaint: Rc<dyn Fn()>) -> Option<gtk::Box> {
    let (editor, status) = with_state(host, |s| (s.editor.clone(), s.status.clone()));
    let editor = editor?;
    let lookup = crate::i18n::strings::lookup;
    let view = editor.view();

    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    root.set_margin_top(6);
    root.set_margin_bottom(6);
    root.add_css_class("card");
    crate::testid::set_test_id(&root, ids::FEATURE_POLICY_EDITOR);

    let title = gtk::Label::new(Some(&view.title.resolve_nested(lookup)));
    title.set_halign(gtk::Align::Start);
    title.add_css_class("heading");
    crate::testid::set_test_id(&title, ids::FEATURE_POLICY_EDITOR_TITLE);
    root.append(&title);

    // A one-of-two radio pair — exactly two choices, never a third "Limit":
    // whether On carries bounds is the cells' business.
    let on_radio = gtk::CheckButton::with_label(f::EDITOR_ON);
    let off_radio = gtk::CheckButton::with_label(f::EDITOR_OFF);
    off_radio.set_group(Some(&on_radio));
    on_radio.set_active(view.on);
    off_radio.set_active(!view.on);
    crate::testid::set_test_attr(&on_radio, "state", if view.on { "on" } else { "off" });
    crate::testid::set_test_attr(&off_radio, "state", if view.on { "off" } else { "on" });
    crate::testid::set_test_id(&on_radio, ids::FEATURE_POLICY_EDITOR_ON_RADIO);
    crate::testid::set_test_id(&off_radio, ids::FEATURE_POLICY_EDITOR_OFF_RADIO);
    for (radio, on) in [(&on_radio, true), (&off_radio, false)] {
        let repaint = Rc::clone(&repaint);
        radio.connect_toggled(move |btn| {
            if !btn.is_active()
                || with_state(host, |s| s.editor.as_ref().map(|e| e.is_on())) == Some(on)
            {
                return;
            }
            with_state(host, |s| {
                if let Some(editor) = s.editor.as_mut() {
                    editor.set_on(on);
                }
            });
            // The widgets under the handler are rebuilt: do it off the stack.
            let repaint = Rc::clone(&repaint);
            glib::idle_add_local_once(move || repaint());
        });
    }
    root.append(&on_radio);
    root.append(&off_radio);

    // An outer tier already denies: whatever is chosen here changes nothing,
    // and the editor says so rather than refusing anything.
    if let Some(note) = &view.off_note {
        let label = dim_label(&note.resolve(lookup));
        root.append(&label);
    }
    if !view.cells.is_empty() {
        root.append(&dim_label(&view.hint.resolve(lookup)));
    }

    // Shared with every entry's handler, which repaints the notes in place.
    let notes: Rc<RefCell<Vec<gtk::Label>>> = Rc::new(RefCell::new(Vec::new()));
    for (i, cell) in view.cells.iter().enumerate() {
        let cell_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
        crate::testid::set_test_id(&cell_box, ids::FEATURE_POLICY_EDITOR_CELL);

        let label_text = cell.label.resolve_nested(lookup);
        let label = gtk::Label::new(Some(&label_text));
        label.set_halign(gtk::Align::Start);
        crate::testid::set_test_id(&label, ids::FEATURE_POLICY_EDITOR_CELL_LABEL);
        cell_box.append(&label);

        let entry = gtk::Entry::new();
        entry.set_text(&cell.text);
        entry.update_property(&[gtk::accessible::Property::Label(&label_text)]);
        crate::testid::set_test_id(&entry, ids::FEATURE_POLICY_EDITOR_CELL_INPUT);
        cell_box.append(&entry);

        // The note is always built and toggled, so typing never rebuilds the
        // cell (and so never drops the entry's focus).
        let note = dim_label("");
        crate::testid::set_test_id(&note, ids::FEATURE_POLICY_EDITOR_CELL_NOTE);
        paint_note(&note, editor_note_text(cell, lookup));
        cell_box.append(&note);
        notes.borrow_mut().push(note);
        root.append(&cell_box);

        // Connected after the seed above, so building the entry edits nothing.
        let notes = Rc::clone(&notes);
        entry.connect_changed(move |entry| {
            let view = with_state(host, |s| {
                let editor = s.editor.as_mut()?;
                editor.set_cell(i, entry.text().to_string());
                Some(editor.view())
            });
            let Some(view) = view else { return };
            for (note, cell) in notes.borrow().iter().zip(view.cells.iter()) {
                paint_note(note, editor_note_text(cell, crate::i18n::strings::lookup));
            }
        });
    }

    // Save and Remove are `OnlineOnly`: the offline gate greys them from the
    // write kind declared here. Cancel is local and stays live.
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let save = gtk::Button::with_label(f::EDITOR_SAVE);
    save.add_css_class("suggested-action");
    crate::testid::set_test_id(&save, ids::FEATURE_POLICY_EDITOR_SAVE_BUTTON);
    crate::offline_gate::declare_wire_kind(&save, host.write_kind());
    save.connect_clicked(move |_| write(host, false));
    buttons.append(&save);
    if view.can_remove {
        let remove = gtk::Button::with_label(f::EDITOR_REMOVE);
        crate::testid::set_test_id(&remove, ids::FEATURE_POLICY_EDITOR_REMOVE_BUTTON);
        crate::offline_gate::declare_wire_kind(&remove, host.write_kind());
        remove.connect_clicked(move |_| write(host, true));
        buttons.append(&remove);
    }
    let cancel_button = gtk::Button::with_label(f::EDITOR_CANCEL);
    crate::testid::set_test_id(&cancel_button, ids::FEATURE_POLICY_EDITOR_CANCEL_BUTTON);
    {
        let repaint = Rc::clone(&repaint);
        cancel_button.connect_clicked(move |_| {
            cancel(host);
            let repaint = Rc::clone(&repaint);
            glib::idle_add_local_once(move || repaint());
        });
    }
    buttons.append(&cancel_button);
    root.append(&buttons);

    if let Some(status) = status {
        let label = gtk::Label::new(Some(&status));
        label.set_halign(gtk::Align::Start);
        crate::testid::set_test_id(&label, ids::FEATURE_POLICY_EDITOR_STATUS);
        root.append(&label);
    }
    Some(root)
}

/// Save (or, with `remove`, clear) the open editor's document. The shared seam
/// parses the draft: a cell it cannot read dispatches nothing and its reason
/// lands on the page's error banner, exactly as on tui.
fn write(host: Host, remove: bool) {
    let Some(editor) = with_state(host, |s| s.editor.clone()) else {
        return;
    };
    CLIENT.with_borrow(|client| {
        if let Some(client) = client {
            client.write_feature_limit(host, editor, remove);
        }
    });
}

fn paint_note(note: &gtk::Label, text: Option<String>) {
    match text {
        Some(text) => {
            note.set_text(&text);
            note.set_visible(true);
        }
        None => {
            note.set_text("");
            note.set_visible(false);
        }
    }
}

fn dim_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_halign(gtk::Align::Start);
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.add_css_class("dim-label");
    label
}
