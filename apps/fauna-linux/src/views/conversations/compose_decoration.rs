//! Inline markdown styling for the compose `gtk::TextView` (`docs/goal/ui/conversations.md`
//! § Compose-field inline markdown styling). The buffer keeps the **literal markdown
//! source**; this is the *decoration layer* — it applies `gtk::TextTag`s over the source
//! byte ranges from the shared `fauna_core::markdown::decoration_map`, styling content
//! (bold/italic/code/heading/blockquote) and dimming the markers, with the markers on the
//! caret's current line revealed so they can be edited. The tokenizer is shared with the
//! render path (`message_bubble`/`markdown.rs`), so the editor preview and the sent message
//! can never disagree on what is styled.

use fauna_core::markdown::MdDecorationKind;
use gtk::prelude::*;

/// Char offset (GTK `TextIter` unit) of a byte offset into `text`. `decoration_map` returns
/// **byte** offsets (Rust `str`), but GTK iters count **chars**, so every range is converted
/// before it touches the buffer. `byte` must be a char boundary (decoration offsets always
/// are).
pub fn byte_to_char(text: &str, byte: usize) -> i32 {
    text[..byte].chars().count() as i32
}

/// Byte offset of a **char** offset into `text` (the inverse of [`byte_to_char`]) — GTK
/// `cursor_position` is a char offset, but `compose_decoration_plan` wants a byte offset.
pub fn char_to_byte(text: &str, char_off: usize) -> usize {
    text.chars().take(char_off).map(char::len_utf8).sum()
}

/// The style tag name for a content decoration kind; `None` for the marker kinds (handled
/// separately so they can be revealed near the caret rather than always styled).
fn style_tag(kind: MdDecorationKind) -> Option<&'static str> {
    match kind {
        MdDecorationKind::Bold => Some("md-bold"),
        MdDecorationKind::Italic => Some("md-italic"),
        MdDecorationKind::BoldItalic => Some("md-bold-italic"),
        MdDecorationKind::Code => Some("md-code"),
        MdDecorationKind::Link | MdDecorationKind::Image => Some("md-link"),
        MdDecorationKind::Heading => Some("md-heading"),
        MdDecorationKind::Blockquote => Some("md-blockquote"),
        MdDecorationKind::Marker | MdDecorationKind::ListMarker => None,
    }
}

/// Create the compose decoration tags in the buffer's tag table once (idempotent — keyed
/// on `md-bold` existing). Mirrors the bubble's `.md-*` CSS look (style.css) so compose
/// preview and rendered message read the same.
fn ensure_tags(buffer: &gtk::TextBuffer) {
    let table = buffer.tag_table();
    if table.lookup("md-bold").is_some() {
        return;
    }
    table.add(&gtk::TextTag::builder().name("md-bold").weight(700).build());
    table.add(
        &gtk::TextTag::builder()
            .name("md-italic")
            .style(gtk::pango::Style::Italic)
            .build(),
    );
    table.add(
        &gtk::TextTag::builder()
            .name("md-bold-italic")
            .weight(700)
            .style(gtk::pango::Style::Italic)
            .build(),
    );
    table.add(
        &gtk::TextTag::builder()
            .name("md-code")
            .family("monospace")
            .build(),
    );
    table.add(
        &gtk::TextTag::builder()
            .name("md-link")
            .underline(gtk::pango::Underline::Single)
            .foreground("#3584e4")
            .build(),
    );
    table.add(
        &gtk::TextTag::builder()
            .name("md-heading")
            .weight(700)
            .scale(1.3)
            .build(),
    );
    table.add(
        &gtk::TextTag::builder()
            .name("md-blockquote")
            .style(gtk::pango::Style::Italic)
            .foreground("#737373")
            .build(),
    );
    // The quote INDENT (the bubble's `.md-blockquote` is indented 12px, style.css).
    // A left margin is a paragraph attribute in GTK — the layout reads it off the
    // tags on the line's first character, which is the `> ` marker, never the
    // content `md-blockquote` covers — so it is its own tag, spanning the whole line.
    table.add(
        &gtk::TextTag::builder()
            .name("md-quote-indent")
            .left_margin(12)
            .build(),
    );
    // Markers: dimmed when *not* on the caret's line; on the active line they keep the
    // default foreground (revealed) so the user can edit the raw `*`/`#`/`>`/… source.
    table.add(
        &gtk::TextTag::builder()
            .name("md-marker")
            .foreground("#737373")
            .build(),
    );
    // Hidden inline markers (the hide-by-default mode): rendered invisible. The caret-edge
    // reveal re-classifies the run's markers as `md-marker` (visible) the moment the caret
    // reaches it, so a hidden marker is never an un-editable trap.
    table.add(
        &gtk::TextTag::builder()
            .name("md-hidden")
            .invisible(true)
            .build(),
    );
}

/// Re-style the whole compose buffer from the shared decoration map. Called on every text
/// change *and* every caret move (the reveal depends on the caret). Clears our tags first,
/// then applies content styles and the marker treatment for the current mode. Idempotent and
/// re-entrancy-safe: applying/removing tags emits only `apply-tag`/`remove-tag`, never
/// `changed` or `cursor-position`, so it does not re-trigger itself.
///
/// `markers_shown` is the per-editor toggle (`markdown-marker-toggle-button`;
/// tracked internally): the **default `false`** HIDES
/// inline emphasis markers (`md-hidden`, revealed at the caret edge) and dims structural
/// prefixes; `true` falls back to the all-dimmed live-preview (markers shown, whole-line reveal).
/// Content styling is identical in both modes.
pub fn apply(buffer: &gtk::TextBuffer, markers_shown: bool) {
    ensure_tags(buffer);
    let start = buffer.start_iter();
    let end = buffer.end_iter();
    buffer.remove_all_tags(&start, &end);

    // `true` = include hidden chars: decorate the full markdown SOURCE (the marker byte ranges
    // come from it). `remove_all_tags` above already cleared the `md-hidden` invisibility, so
    // this is equivalent today, but reading the full source is order-independent + matches the
    // rule that every compose body read is full-source (only the automation `text_of` is visible).
    let text = buffer.text(&start, &end, true).to_string();
    let decos = fauna_core::markdown::decoration_map(&text);
    if decos.is_empty() {
        return;
    }

    // Content styles (both modes): bold/italic/code/link/heading/blockquote.
    for d in &decos {
        if let Some(tag) = style_tag(d.kind) {
            let si = buffer.iter_at_offset(byte_to_char(&text, d.start));
            let ei = buffer.iter_at_offset(byte_to_char(&text, d.end));
            buffer.apply_tag_by_name(tag, &si, &ei);
            if matches!(d.kind, MdDecorationKind::Blockquote) {
                let mut line_start = si;
                line_start.set_line_offset(0);
                buffer.apply_tag_by_name("md-quote-indent", &line_start, &ei);
            }
        }
    }

    if markers_shown {
        // "Show markers" mode: dim every marker off the caret's line (the markers on the active
        // line keep the default foreground so the raw `*`/`#`/`>`/fence source can be edited).
        // The caret-line reveal rule is the shared
        // `fauna_core::markdown::compose_show_markers_dim_ranges` — the whole-line counterpart of
        // `compose_decoration_plan`, one policy for windows/android/linux (priority #2/#4) — and
        // is applied exactly like the hide-branch's `plan.dim` below.
        let caret_byte = char_to_byte(&text, buffer.cursor_position() as usize);
        for r in &fauna_core::markdown::compose_show_markers_dim_ranges(&text, caret_byte) {
            let si = buffer.iter_at_offset(byte_to_char(&text, r.start));
            let ei = buffer.iter_at_offset(byte_to_char(&text, r.end));
            buffer.apply_tag_by_name("md-marker", &si, &ei);
        }
        return;
    }

    // Hide-by-default mode: the shared plan decides which markers conceal vs dim (inline
    // emphasis hidden + caret-edge reveal; structural prefixes dimmed off the caret line). One
    // policy for web + native — `fauna_core::markdown::compose_decoration_plan` (priority #2).
    let caret_byte = char_to_byte(&text, buffer.cursor_position() as usize);
    let plan = fauna_core::markdown::compose_decoration_plan(&text, caret_byte);
    for r in &plan.hide {
        let si = buffer.iter_at_offset(byte_to_char(&text, r.start));
        let ei = buffer.iter_at_offset(byte_to_char(&text, r.end));
        buffer.apply_tag_by_name("md-hidden", &si, &ei);
    }
    for r in &plan.dim {
        let si = buffer.iter_at_offset(byte_to_char(&text, r.start));
        let ei = buffer.iter_at_offset(byte_to_char(&text, r.end));
        buffer.apply_tag_by_name("md-marker", &si, &ei);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_to_char_handles_multibyte() {
        assert_eq!(byte_to_char("hello", 5), 5);
        // "café" — é is 2 bytes, so byte 5 (just past é) is char index 4.
        assert_eq!(byte_to_char("café x", 5), 4);
        assert_eq!(byte_to_char("café x", 0), 0);
    }

    #[test]
    fn char_to_byte_is_the_inverse_of_byte_to_char() {
        assert_eq!(char_to_byte("hello", 5), 5);
        // "café x" — char index 4 (just past é) is byte 5 (é is 2 bytes).
        assert_eq!(char_to_byte("café x", 4), 5);
        assert_eq!(char_to_byte("café x", 0), 0);
        // round-trips with byte_to_char on a char boundary.
        assert_eq!(byte_to_char("café x", char_to_byte("café x", 4)), 4);
    }

    #[test]
    fn style_tag_maps_kinds() {
        // Marker kinds are handled by the reveal path, not a content style.
        assert!(style_tag(MdDecorationKind::Marker).is_none());
        assert!(style_tag(MdDecorationKind::ListMarker).is_none());
        assert_eq!(style_tag(MdDecorationKind::Bold), Some("md-bold"));
        assert_eq!(
            style_tag(MdDecorationKind::BoldItalic),
            Some("md-bold-italic")
        );
        assert_eq!(style_tag(MdDecorationKind::Heading), Some("md-heading"));
        assert_eq!(style_tag(MdDecorationKind::Image), Some("md-link"));
    }
}
