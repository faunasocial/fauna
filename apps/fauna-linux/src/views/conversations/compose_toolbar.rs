//! Markdown compose toolbar + keyboard shortcuts for the inline compose area.
//!
//! Provides the compact toolbar that inserts markdown markers into a
//! `gtk::TextView`, plus keyboard shortcut wiring. (A separate "full" toolbar for
//! a long-form compose dialog existed briefly but the dialog it served was later
//! folded into this inline compose bar, leaving it dead code — removed
//! 2026-07-17; heading/list moved into the one toolbar actually wired up.)

use fauna_ui_ids as ids;
use gtk::prelude::*;
use std::cell::Cell;
use std::rc::Rc;

// ---------------------------------------------------------------------------
// Text manipulation helpers
// ---------------------------------------------------------------------------

/// Wrap the current selection with markdown `prefix`/`suffix` markers via the shared
/// `fauna_core::markdown::wrap_selection` rule, which keeps edge whitespace OUTSIDE the
/// markers — so a double-click word-selection's trailing space can't produce `*italic *`
/// (which collides with an adjacent `**bold**` into the invalid `*italic ***bold**`).
/// `placeholder` fills an empty/all-whitespace selection (`"text"` ⇒ `*text*` with "text"
/// re-selected, uniform with web/windows). The pure offset/splice math lives in
/// [`apply_wrap`] (the linux twin of android `applyWrap` / windows
/// `MarkdownAuthoring.WrapSelection`); this only reads the buffer and applies the result.
fn wrap_selection(text_view: &gtk::TextView, prefix: &str, suffix: &str, placeholder: &str) {
    let buffer = text_view.buffer();
    let (sel_start, sel_end) = match buffer.selection_bounds() {
        Some((s, e)) => (s.offset(), e.offset()),
        None => {
            let c = buffer.cursor_position();
            (c, c)
        }
    };
    // `true` = include hidden chars: the wrap splices into the full markdown SOURCE and then
    // `set_text`s the result, so it must read the full buffer (incl. `md-hidden`-concealed
    // markers) — `false` would drop every concealed marker on the first toolbar wrap. The
    // char-offset selection is over the full buffer too, so the offsets stay aligned.
    let text = buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), true)
        .to_string();
    let (new_text, sel, len) = apply_wrap(
        &text,
        sel_start as usize,
        sel_end as usize,
        prefix,
        suffix,
        placeholder,
    );
    buffer.set_text(&new_text);
    // GTK `TextIter` offsets are char offsets, matching `apply_wrap`'s return units.
    let s = buffer.iter_at_offset(sel as i32);
    let e = buffer.iter_at_offset((sel + len) as i32);
    buffer.select_range(&s, &e);
    text_view.grab_focus();
}

/// Pure splice for [`wrap_selection`]: given the buffer `text` and a **char-offset**
/// selection `[sel_start, sel_end)`, run the shared `wrap_selection` rule and return the
/// new full text plus the char-offset selection re-covering the wrapped core. GTK-free so
/// it is unit-testable, and char-offset based to match GTK `TextIter`. Twin of android
/// `applyWrap` and windows `MarkdownAuthoring.WrapSelection` (both UTF-16); the wrap *rule*
/// itself is the one shared `fauna_core::markdown::wrap_selection`.
fn apply_wrap(
    text: &str,
    sel_start: usize,
    sel_end: usize,
    prefix: &str,
    suffix: &str,
    placeholder: &str,
) -> (String, usize, usize) {
    let chars: Vec<char> = text.chars().collect();
    let selected: String = chars[sel_start..sel_end].iter().collect();
    let w = fauna_core::markdown::wrap_selection(&selected, prefix, suffix, placeholder);
    let before: String = chars[..sel_start].iter().collect();
    let after: String = chars[sel_end..].iter().collect();
    let new_text = format!("{before}{}{after}", w.replacement);
    let new_sel = sel_start + w.before_core.chars().count();
    let new_len = w.core.chars().count();
    (new_text, new_sel, new_len)
}

/// Insert `prefix` at the beginning of the current line.
fn prefix_line(text_view: &gtk::TextView, prefix: &str) {
    let buffer = text_view.buffer();
    let cursor = buffer.cursor_position();
    let mut iter = buffer.iter_at_offset(cursor);
    iter.set_line_offset(0);
    buffer.insert(&mut iter, prefix);
    text_view.grab_focus();
}

// A markdown link is just the shared wrap with the `[`…`](url)` markers and a `"text"`
// placeholder (mirrors web/android): a selection becomes `[selected](url)` with the label
// re-selected, an empty selection becomes `[text](url)` with "text" re-selected. So the
// Link button/shortcut route through [`wrap_selection`] like every other inline marker
// rather than a bespoke link splice.

// ---------------------------------------------------------------------------
// Button factory
// ---------------------------------------------------------------------------

/// Create a flat toolbar button with a label.
fn toolbar_button(label: &str) -> gtk::Button {
    let btn = gtk::Button::with_label(label);
    btn.add_css_class("flat");
    btn.set_focusable(false); // Don't steal focus from the TextView.
    btn
}

/// The per-editor marker-visibility toggle (`markdown-marker-toggle-button`;
/// tracked internally): flips `markers_shown` and
/// re-decorates the buffer. Default un-pressed = markers hidden (the new compose default);
/// pressed = the all-dimmed live-preview. The `∗` glyph mirrors the web toggle.
fn marker_toggle_button(
    text_view: &gtk::TextView,
    markers_shown: Rc<Cell<bool>>,
) -> gtk::ToggleButton {
    let toggle = gtk::ToggleButton::with_label("∗");
    toggle.add_css_class("flat");
    toggle.set_focusable(false); // Don't steal focus from the TextView.
    toggle.set_tooltip_text(Some(crate::i18n::strings::markdown::TOGGLE_MARKERS));
    toggle.set_active(markers_shown.get()); // default hidden ⇒ not pressed
    crate::testid::set_test_id(&toggle, ids::MARKDOWN_MARKER_TOGGLE_BUTTON);
    let tv = text_view.clone();
    toggle.connect_toggled(move |b| {
        markers_shown.set(b.is_active());
        super::compose_decoration::apply(&tv.buffer(), markers_shown.get());
    });
    toggle
}

// ---------------------------------------------------------------------------
// Compact toolbar (for inline compose)
// ---------------------------------------------------------------------------

/// Build a compact formatting toolbar suitable for the inline compose area.
///
/// Buttons: Bold, Italic, Code, Link, Heading, List, Quote, and the marker-visibility
/// toggle. `markers_shown` is the per-editor toggle state shared with the buffer's
/// decoration `apply` (default hidden).
pub fn build_compact_toolbar(text_view: &gtk::TextView, markers_shown: Rc<Cell<bool>>) -> gtk::Box {
    // Explicit Toolbar role — a non-generic accessible role keeps the
    // widget in the AT-SPI tree even when it's insensitive (an insensitive
    // GENERIC-role gtk::Box drops out under Mutter, breaking the e2e
    // `get_attr("markdown-toolbar", "disabled")` read). Also semantically
    // correct: this is a toolbar.
    let toolbar: gtk::Box = glib::Object::builder()
        .property("orientation", gtk::Orientation::Horizontal)
        .property("spacing", 2)
        .property("accessible-role", gtk::AccessibleRole::Toolbar)
        .build();
    toolbar.set_margin_start(4);
    toolbar.set_margin_end(4);
    toolbar.set_margin_bottom(2);
    crate::testid::set_test_id(&toolbar, ids::MARKDOWN_TOOLBAR);

    // Bold
    let btn = toolbar_button("B");
    btn.add_css_class("bold");
    crate::testid::set_test_id(&btn, ids::MARKDOWN_BOLD_BUTTON);
    let tv = text_view.clone();
    btn.connect_clicked(move |_| wrap_selection(&tv, "**", "**", "text"));
    toolbar.append(&btn);

    // Italic
    let btn = toolbar_button("I");
    crate::testid::set_test_id(&btn, ids::MARKDOWN_ITALIC_BUTTON);
    let tv = text_view.clone();
    btn.connect_clicked(move |_| wrap_selection(&tv, "*", "*", "text"));
    toolbar.append(&btn);

    // Code
    let btn = toolbar_button("</>");
    crate::testid::set_test_id(&btn, ids::MARKDOWN_CODE_BUTTON);
    let tv = text_view.clone();
    btn.connect_clicked(move |_| wrap_selection(&tv, "`", "`", "text"));
    toolbar.append(&btn);

    // Link
    let btn = toolbar_button("Link");
    crate::testid::set_test_id(&btn, ids::MARKDOWN_LINK_BUTTON);
    let tv = text_view.clone();
    btn.connect_clicked(move |_| wrap_selection(&tv, "[", "](url)", "text"));
    toolbar.append(&btn);

    // Heading — matches web's H2 prefix (`MarkdownToolbar.svelte` `insert('## ')`)
    // exactly, including the wrap-with-placeholder behavior on an empty selection
    // (`## text`), for cross-app uniformity (priority #1) rather than a
    // linux-only prefix-only H1 (the previous, unreachable `build_full_toolbar`).
    let btn = toolbar_button("H");
    crate::testid::set_test_id(&btn, ids::MARKDOWN_HEADING_BUTTON);
    let tv = text_view.clone();
    btn.connect_clicked(move |_| wrap_selection(&tv, "## ", "", "text"));
    toolbar.append(&btn);

    // List (unordered) — matches web's `insert('- ')`.
    let btn = toolbar_button("List");
    crate::testid::set_test_id(&btn, ids::MARKDOWN_LIST_BUTTON);
    let tv = text_view.clone();
    btn.connect_clicked(move |_| wrap_selection(&tv, "- ", "", "text"));
    toolbar.append(&btn);

    // Quote
    let btn = toolbar_button("Quote");
    let tv = text_view.clone();
    btn.connect_clicked(move |_| prefix_line(&tv, "> "));
    toolbar.append(&btn);

    // Marker-visibility toggle (hide-by-default; press to reveal the dimmed markers).
    toolbar.append(&marker_toggle_button(text_view, markers_shown));

    toolbar
}

// ---------------------------------------------------------------------------
// Keyboard shortcuts
// ---------------------------------------------------------------------------

/// Attach markdown keyboard shortcuts to a compose `TextView`.
///
/// - Ctrl+B: bold
/// - Ctrl+I: italic
/// - Ctrl+Shift+C: inline code
/// - Ctrl+K: link
/// - Ctrl+Shift+Q: blockquote prefix
pub fn setup_compose_shortcuts(text_view: &gtk::TextView) {
    let key_controller = gtk::EventControllerKey::new();
    let tv = text_view.clone();

    key_controller.connect_key_pressed(move |_, key, _, modifier| {
        let ctrl = modifier.contains(gtk::gdk::ModifierType::CONTROL_MASK);
        let shift = modifier.contains(gtk::gdk::ModifierType::SHIFT_MASK);

        if !ctrl {
            return glib::Propagation::Proceed;
        }

        match key {
            gtk::gdk::Key::b | gtk::gdk::Key::B if !shift => {
                wrap_selection(&tv, "**", "**", "text");
                glib::Propagation::Stop
            }
            gtk::gdk::Key::i | gtk::gdk::Key::I if !shift => {
                wrap_selection(&tv, "*", "*", "text");
                glib::Propagation::Stop
            }
            gtk::gdk::Key::c | gtk::gdk::Key::C if shift => {
                wrap_selection(&tv, "`", "`", "text");
                glib::Propagation::Stop
            }
            gtk::gdk::Key::k | gtk::gdk::Key::K if !shift => {
                wrap_selection(&tv, "[", "](url)", "text");
                glib::Propagation::Stop
            }
            gtk::gdk::Key::q | gtk::gdk::Key::Q if shift => {
                prefix_line(&tv, "> ");
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    });

    text_view.add_controller(key_controller);
}

use gtk::glib;

#[cfg(test)]
mod tests {
    use super::apply_wrap;

    // Linux twin of windows `MarkdownAuthoringTests` / android `MarkdownToolbarTest`: lock
    // the shared whitespace rule + the GTK-side (char-offset) splice. `apply_wrap` is
    // GTK-free, so these run without a display.

    #[test]
    fn trailing_space_stays_outside_markers() {
        // Double-click "italic" selects "italic " (trailing space) → the space must end up
        // OUTSIDE the markers: `*italic* `, not `*italic *`.
        let (text, sel, len) = apply_wrap("italic ", 0, 7, "*", "*", "text");
        assert_eq!(text, "*italic* ");
        assert_eq!(sel, 1); // just after the opening `*`
        assert_eq!(len, 6); // "italic"
    }

    #[test]
    fn adjacent_words_italic_then_bold_no_marker_collision() {
        // The exact reported bug, end-to-end: in "italic bold", double-click "italic"
        // (selection picks up the trailing space) → italic, then double-click "bold" →
        // bold. Wrapping the raw selection produced `*italic ***bold**`; the shared wrap
        // yields the correct `*italic* **bold**`.
        let (after_italic, _, _) = apply_wrap("italic bold", 0, 7, "*", "*", "text");
        assert_eq!(after_italic, "*italic* bold");

        let bold_start = after_italic.chars().count() - 4; // "bold" is the last 4 chars
        let (after_bold, _, _) = apply_wrap(
            &after_italic,
            bold_start,
            bold_start + 4,
            "**",
            "**",
            "text",
        );
        assert_eq!(after_bold, "*italic* **bold**");
    }

    #[test]
    fn empty_selection_wraps_placeholder() {
        // Uniform with web/windows: an empty selection yields `**text**` with "text"
        // re-selected (not the old linux cursor-between-markers behavior).
        let (text, sel, len) = apply_wrap("", 0, 0, "**", "**", "text");
        assert_eq!(text, "**text**");
        assert_eq!(sel, 2); // just after `**`
        assert_eq!(len, 4); // "text"
    }

    #[test]
    fn empty_selection_link_inserts_placeholder_label() {
        // The Link button routes through the same rule with `[`…`](url)` markers: an empty
        // selection becomes `[text](url)` with the "text" label re-selected (mirrors web).
        let (text, sel, len) = apply_wrap("", 0, 0, "[", "](url)", "text");
        assert_eq!(text, "[text](url)");
        assert_eq!(sel, 1); // just after `[`
        assert_eq!(len, 4); // "text"
    }

    #[test]
    fn unicode_core_is_char_offset_safe() {
        // Char offsets (GTK `TextIter` units), so a multi-byte core re-selects correctly —
        // a naive byte splice would mis-place the selection.
        let (text, sel, len) = apply_wrap("héllo ", 0, 6, "*", "*", "text");
        assert_eq!(text, "*héllo* ");
        assert_eq!(sel, 1);
        assert_eq!(len, 5); // "héllo" is 5 chars
    }
}
