//! The shared `feature-policy-editor` family's paint — ONE render for both of
//! its hosts (`dynamic-features.md` § Authoring surfaces): the admin Nest page
//! opens it at the admin tier (`crate::admin`), a Feature limits row in Settings
//! at the self tier (`crate::settings`).
//!
//! **This module paints; it decides nothing.** The draft, the cell list and its
//! order, the labels, the no-effect notes, whether *Remove limit* renders and
//! the parser behind *Save* are all `fauna_client_features::PolicyEditor`'s, so
//! the editor cannot differ between the two hosts or across the 7 apps. The
//! hosts differ only in the gestures they bind — which is where the tier (and so
//! the write kind the offline gate reads) is decided.

use fauna_client_features::{PolicyEditor, editor_note_text};
use fauna_i18n::strings::features as f;
use fauna_ui_ids as ids;

use crate::element::{Element, Field, Gesture};

/// The host's bindings for the editor's controls.
pub struct EditorGestures {
    pub on: Gesture,
    pub off: Gesture,
    pub save: Gesture,
    pub remove: Gesture,
    pub cancel: Gesture,
    /// The field behind `feature-policy-editor-cell-input[index]`.
    pub cell: fn(usize) -> Field,
}

/// The open editor's elements, in paint order. `status` is the host's last
/// verdict line (`feature-policy-editor-status`), present only after a write.
pub fn editor_elements(
    editor: &PolicyEditor,
    status: Option<&str>,
    gestures: EditorGestures,
) -> Vec<Element> {
    let lookup = fauna_i18n::strings::lookup;
    let view = editor.view();
    let mut els = vec![
        Element::label(ids::FEATURE_POLICY_EDITOR, " "),
        Element::label(
            ids::FEATURE_POLICY_EDITOR_TITLE,
            view.title.resolve_nested(lookup),
        ),
        // A one-of-two `Radio` pair (`apps/tui.md` § Rendering → *Control
        // vocabulary*, rule 1): exactly two choices, never a third "Limit" —
        // whether On carries bounds is the cells' business.
        Element::radio_gesture(
            ids::FEATURE_POLICY_EDITOR_ON_RADIO,
            f::EDITOR_ON,
            view.on,
            gestures.on,
        )
        .attr("state", if view.on { "on" } else { "off" }),
        Element::radio_gesture(
            ids::FEATURE_POLICY_EDITOR_OFF_RADIO,
            f::EDITOR_OFF,
            !view.on,
            gestures.off,
        )
        .attr("state", if view.on { "off" } else { "on" }),
    ];
    // An outer tier already denies: whatever is chosen here changes nothing,
    // and the editor says so rather than refusing anything.
    if let Some(note) = &view.off_note {
        els.push(Element::chrome(note.resolve(lookup)));
    }
    if !view.cells.is_empty() {
        els.push(Element::chrome(view.hint.resolve(lookup)));
    }
    for (i, cell) in view.cells.iter().enumerate() {
        els.push(
            Element::label(ids::FEATURE_POLICY_EDITOR_CELL, " ")
                .within(ids::FEATURE_POLICY_EDITOR_CELL, i),
        );
        let label = cell.label.resolve_nested(lookup);
        els.push(
            Element::label(ids::FEATURE_POLICY_EDITOR_CELL_LABEL, label.clone())
                .within(ids::FEATURE_POLICY_EDITOR_CELL, i),
        );
        els.push(
            Element::input(
                ids::FEATURE_POLICY_EDITOR_CELL_INPUT,
                cell.text.clone(),
                (gestures.cell)(i),
            )
            .labelled(label)
            .within(ids::FEATURE_POLICY_EDITOR_CELL, i),
        );
        if let Some(note) = editor_note_text(cell, lookup) {
            els.push(
                Element::label(ids::FEATURE_POLICY_EDITOR_CELL_NOTE, note)
                    .within(ids::FEATURE_POLICY_EDITOR_CELL, i),
            );
        }
    }
    // Save and Remove are `OnlineOnly`: the offline gate greys them from the
    // write kind the host's gesture declares. Cancel is local and stays live.
    els.push(Element::gesture_button(
        ids::FEATURE_POLICY_EDITOR_SAVE_BUTTON,
        f::EDITOR_SAVE,
        true,
        gestures.save,
    ));
    if view.can_remove {
        els.push(Element::gesture_button(
            ids::FEATURE_POLICY_EDITOR_REMOVE_BUTTON,
            f::EDITOR_REMOVE,
            true,
            gestures.remove,
        ));
    }
    els.push(Element::gesture_button(
        ids::FEATURE_POLICY_EDITOR_CANCEL_BUTTON,
        f::EDITOR_CANCEL,
        true,
        gestures.cancel,
    ));
    if let Some(status) = status {
        els.push(Element::label(ids::FEATURE_POLICY_EDITOR_STATUS, status));
    }
    els
}
