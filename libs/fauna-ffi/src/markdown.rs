//! UniFFI façade for the shared NIP-23 Markdown parser
//! (`fauna_core::markdown::parse_markdown`). Gives Apple / Windows / Android the same
//! block/line/span token model the Rust-native Linux app would read directly, so each
//! native app renders Markdown from one definition instead of re-deriving its own
//! subset (priority #1/#2/#4). The web app renders the same parser's HTML via the
//! `fauna-wasm` `markdownToHtml` face.
//!
//! The return types are **fauna-ffi-local** `uniffi::Record`s (not `fauna_core` types) so
//! uniffi-bindgen-go emits a self-contained Go binding — no bare-`fauna_core` cross-
//! namespace import (the footgun that forces `value_format` behind a feature gate). The
//! Go mail bridge never renders Markdown, but keeping the export gate-free is harmless.

use fauna_core::markdown;

/// One inline run: text plus at most one active style; `link` is the href when this run
/// is a `[label](url)` link, `image_url` the source when it is a `![alt](url)` **remote**
/// image (`text` holds the alt), else empty. The image url is **un-fetched** — the native
/// app renders it blocked-by-default and reveals it on a per-message opt-in (the
/// `load-remote-content-button`). Mirror of [`markdown::MdSpan`].
#[derive(uniffi::Record)]
pub struct FfiMdSpan {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub link: String,
    pub image_url: String,
}

/// One line of inline content (heading text, paragraph, or list item). Mirror of
/// [`markdown::MdLine`].
#[derive(uniffi::Record)]
pub struct FfiMdLine {
    pub spans: Vec<FfiMdSpan>,
}

/// A block node. `kind` is `"heading"` | `"paragraph"` | `"list"` | `"ordered_list"` |
/// `"blockquote"` | `"code_block"`; `level` is the heading level (1–4; 0 otherwise);
/// `code` is the raw fenced-code text; `lines` is the inline content (1 line for
/// heading/paragraph/blockquote, N for a list / ordered list, empty for a code block).
/// An ordered list carries no item numbers — it renumbers from 1 at render. Mirror of
/// [`markdown::MdBlock`].
#[derive(uniffi::Record)]
pub struct FfiMdBlock {
    pub kind: String,
    pub level: u8,
    pub code: String,
    pub lines: Vec<FfiMdLine>,
}

impl From<markdown::MdSpan> for FfiMdSpan {
    fn from(s: markdown::MdSpan) -> Self {
        FfiMdSpan {
            text: s.text,
            bold: s.bold,
            italic: s.italic,
            code: s.code,
            link: s.link,
            image_url: s.image_url,
        }
    }
}

impl From<markdown::MdLine> for FfiMdLine {
    fn from(l: markdown::MdLine) -> Self {
        FfiMdLine {
            spans: l.spans.into_iter().map(FfiMdSpan::from).collect(),
        }
    }
}

impl From<markdown::MdBlock> for FfiMdBlock {
    fn from(b: markdown::MdBlock) -> Self {
        FfiMdBlock {
            kind: b.kind,
            level: b.level,
            code: b.code,
            lines: b.lines.into_iter().map(FfiMdLine::from).collect(),
        }
    }
}

/// UniFFI face of [`fauna_core::markdown::parse_markdown`] — parse a NIP-23 article body
/// into the block/line/span token model the native app renders.
#[uniffi::export]
pub fn parse_markdown(md: String) -> Vec<FfiMdBlock> {
    markdown::parse_markdown(&md)
        .into_iter()
        .map(FfiMdBlock::from)
        .collect()
}

/// One inline-styling decoration over a byte range of the compose buffer's **raw** source
/// (`conversations.md` § Compose-field inline markdown styling). `kind` is the snake_case
/// [`markdown::MdDecorationKind::as_str`] token (`"marker"`, `"bold"`, `"italic"`,
/// `"bold_italic"`, `"code"`, `"link"`, `"image"`, `"heading"`, `"blockquote"`,
/// `"list_marker"`); `level` is the heading level (1–4) when `kind == "heading"`, else 0.
/// The native app converts the byte range to its widget's offset unit and applies a
/// styled (content) or dimmed (marker) run. Mirror of [`markdown::MdDecoration`].
#[derive(uniffi::Record)]
pub struct FfiMdDecoration {
    pub start: u64,
    pub end: u64,
    pub kind: String,
    pub level: u8,
}

impl From<markdown::MdDecoration> for FfiMdDecoration {
    fn from(d: markdown::MdDecoration) -> Self {
        FfiMdDecoration {
            start: d.start as u64,
            end: d.end as u64,
            kind: d.kind.as_str().to_string(),
            level: d.level,
        }
    }
}

/// UniFFI mirror of [`fauna_core::markdown::SelectionWrap`] — the result of wrapping a
/// compose-toolbar selection in inline markers (see [`wrap_markdown_selection`]). The
/// caller splices `replacement` over the selection, then re-selects `core` by shifting
/// the selection start past `before_core`'s native length.
#[cfg(feature = "markdown-authoring")]
#[derive(uniffi::Record)]
pub struct FfiSelectionWrap {
    pub replacement: String,
    pub before_core: String,
    pub core: String,
}

#[cfg(feature = "markdown-authoring")]
impl From<markdown::SelectionWrap> for FfiSelectionWrap {
    fn from(w: markdown::SelectionWrap) -> Self {
        FfiSelectionWrap {
            replacement: w.replacement,
            before_core: w.before_core,
            core: w.core,
        }
    }
}

/// UniFFI face of [`fauna_core::markdown::decoration_map`] — the compose field's inline
/// markdown styling map (styled-content + marker byte ranges over the raw source) for the
/// Windows / macOS / iOS / Android compose appliers.
#[uniffi::export]
pub fn decoration_map(src: String) -> Vec<FfiMdDecoration> {
    markdown::decoration_map(&src)
        .into_iter()
        .map(FfiMdDecoration::from)
        .collect()
}

/// One marker byte range into the compose buffer's **raw** source (same space as
/// [`decoration_map`] / [`FfiMdDecoration`]). The native app maps `[start, end)` to its
/// widget's offset unit (UTF-16 chars on Windows RichEditBox / Apple `NSTextView`, code
/// points on Android `VisualTransformation`) and conceals or dims that run. Mirror of
/// [`markdown::RevealSpan`].
#[derive(uniffi::Record)]
pub struct FfiRevealSpan {
    pub start: u64,
    pub end: u64,
}

impl From<markdown::RevealSpan> for FfiRevealSpan {
    fn from(s: markdown::RevealSpan) -> Self {
        FfiRevealSpan {
            start: s.start as u64,
            end: s.end as u64,
        }
    }
}

/// The compose-field **marker treatment** for a hide-by-default (Notes-fidelity) editor —
/// the byte ranges to CONCEAL (`hide`) vs show DIMMED (`dim`), for a caret at the offset
/// passed to [`compose_decoration_plan`]. Inline emphasis markers (`*`/`**`/`` ` ``/`[]()`)
/// hide unless the caret is in their run; structural prefixes (heading/quote/list) dim off
/// the caret's line. Content styling is unchanged — the client styles the non-marker
/// [`decoration_map`] kinds as before; this only decides marker visibility. Mirror of
/// [`markdown::ComposeMarkerPlan`].
#[derive(uniffi::Record)]
pub struct FfiComposeMarkerPlan {
    pub hide: Vec<FfiRevealSpan>,
    pub dim: Vec<FfiRevealSpan>,
}

impl From<markdown::ComposeMarkerPlan> for FfiComposeMarkerPlan {
    fn from(p: markdown::ComposeMarkerPlan) -> Self {
        FfiComposeMarkerPlan {
            hide: p.hide.into_iter().map(FfiRevealSpan::from).collect(),
            dim: p.dim.into_iter().map(FfiRevealSpan::from).collect(),
        }
    }
}

/// UniFFI face of [`fauna_core::markdown::compose_decoration_plan`] — the compose field's
/// **hide-by-default** marker plan (inline emphasis concealed + caret-edge reveal; structural
/// prefixes dimmed) for the Windows / macOS / iOS / Android compose appliers in their
/// markers-hidden mode (`markdown-marker-toggle-button` default). The web (CodeMirror) + Linux
/// (GTK) appliers consume the same `compose_decoration_plan` directly (Rust-native /
/// WASM-faced); the natives consume it here so all six share one policy. `caret`
/// is a byte offset into `src`.
#[uniffi::export]
pub fn compose_decoration_plan(src: String, caret: u64) -> FfiComposeMarkerPlan {
    markdown::compose_decoration_plan(&src, caret as usize).into()
}

/// UniFFI face of [`fauna_core::markdown::compose_show_markers_dim_ranges`] — the compose field's
/// **"show markers" (dimmed live-preview) mode** dim set: the marker byte-ranges to draw DIMMED
/// (every syntax marker whose source line differs from the caret's line; markers on the caret's
/// line are revealed at full opacity so the raw markdown is editable). The whole-line counterpart
/// of [`compose_decoration_plan`]'s hide-by-default per-run reveal, for the Windows / Android
/// compose appliers in their markers-shown mode (`markdown-marker-toggle-button`). Linux (GTK) consumes
/// `compose_show_markers_dim_ranges` directly (Rust-native); the natives consume it here so the
/// three share one caret-line reveal rule. `caret` is a byte offset into `src`;
/// the returned `[start, end)` ranges are byte offsets too (same space as [`decoration_map`]).
///
/// Gated behind `markdown-authoring` (like [`wrap_markdown_selection`]) so the Go mail-bridge
/// `--no-default-features` build drops it — the bridge has no compose editor, and gating keeps the
/// checked-in Go bindings byte-identical (no off-win Go regen).
#[cfg(feature = "markdown-authoring")]
#[uniffi::export]
pub fn compose_show_markers_dim_ranges(src: String, caret: u64) -> Vec<FfiRevealSpan> {
    markdown::compose_show_markers_dim_ranges(&src, caret as usize)
        .into_iter()
        .map(FfiRevealSpan::from)
        .collect()
}

/// UniFFI face of [`fauna_core::markdown::wrap_selection`] — wrap a compose-toolbar
/// selection in `prefix`/`suffix` markers, keeping edge whitespace OUTSIDE them so
/// adjacent emphasis spans don't collide into `*italic ***bold**`. The native compose
/// toolbars (Apple/Android/Windows) call this instead of each re-deriving the wrap
/// (priority #1/#2/#4). Empty/all-whitespace selection wraps `placeholder` (`""` ⇒
/// cursor between the markers).
#[cfg(feature = "markdown-authoring")]
#[uniffi::export]
pub fn wrap_markdown_selection(
    selected: String,
    prefix: String,
    suffix: String,
    placeholder: String,
) -> FfiSelectionWrap {
    markdown::wrap_selection(&selected, &prefix, &suffix, &placeholder).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_block_maps_through() {
        let blocks = parse_markdown("## Hi **b**".to_string());
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, "heading");
        assert_eq!(blocks[0].level, 2);
        assert_eq!(blocks[0].lines[0].spans[1].text, "b");
        assert!(blocks[0].lines[0].spans[1].bold);
    }

    #[test]
    fn link_span_carries_href() {
        let blocks = parse_markdown("[t](https://x.io)".to_string());
        let span = &blocks[0].lines[0].spans[0];
        assert_eq!(span.text, "t");
        assert_eq!(span.link, "https://x.io");
    }

    #[test]
    fn blockquote_and_ordered_list_kinds_map_through() {
        // The new superset kinds flow through the façade unchanged (same `kind`
        // strings; no struct change → no binding regen).
        let bq = parse_markdown("> quoted".to_string());
        assert_eq!(bq[0].kind, "blockquote");
        assert_eq!(bq[0].lines[0].spans[0].text, "quoted");

        let ol = parse_markdown("1. one\n2. two".to_string());
        assert_eq!(ol.len(), 1);
        assert_eq!(ol[0].kind, "ordered_list");
        assert_eq!(ol[0].lines.len(), 2);
    }

    #[test]
    fn bold_italic_span_sets_both_flags() {
        let blocks = parse_markdown("***x***".to_string());
        let span = &blocks[0].lines[0].spans[0];
        assert_eq!(span.text, "x");
        assert!(span.bold);
        assert!(span.italic);
    }

    #[test]
    fn image_span_carries_unfetched_src() {
        // `![alt](url)` maps through as an image span (alt in `text`, src in
        // `image_url`) so native apps render it blocked-by-default.
        let blocks = parse_markdown("![cat](https://img.test/c.png)".to_string());
        let span = &blocks[0].lines[0].spans[0];
        assert_eq!(span.text, "cat");
        assert_eq!(span.image_url, "https://img.test/c.png");
        assert!(span.link.is_empty());
    }

    #[test]
    fn decoration_map_maps_through_with_snake_case_kinds() {
        // The compose inline-styling map flows through the façade as flat records: byte
        // offsets (u64) + snake_case `kind` + heading `level`.
        let decos = decoration_map("hello *world*".to_string());
        assert_eq!(decos.len(), 3);
        assert_eq!(
            (decos[0].start, decos[0].end, decos[0].kind.as_str()),
            (6, 7, "marker")
        );
        assert_eq!(
            (decos[1].start, decos[1].end, decos[1].kind.as_str()),
            (7, 12, "italic")
        );
        assert_eq!(
            (decos[2].start, decos[2].end, decos[2].kind.as_str()),
            (12, 13, "marker")
        );

        // Heading carries its level on the record (kind == "heading").
        let h = decoration_map("# Hi".to_string());
        assert_eq!((h[1].kind.as_str(), h[1].level), ("heading", 1));
    }

    /// The face must mirror `fauna_core` faithfully (the same byte ranges, `usize`→`u64`), so
    /// every test asserts the FFI plan equals the core plan for the same `(src, caret)`.
    fn assert_mirrors_core(src: &str, caret: u64) -> FfiComposeMarkerPlan {
        let core = markdown::compose_decoration_plan(src, caret as usize);
        let ffi = compose_decoration_plan(src.to_string(), caret);
        let want = |v: &[markdown::RevealSpan]| -> Vec<(u64, u64)> {
            v.iter().map(|s| (s.start as u64, s.end as u64)).collect()
        };
        let got = |v: &[FfiRevealSpan]| -> Vec<(u64, u64)> {
            v.iter().map(|s| (s.start, s.end)).collect()
        };
        assert_eq!(got(&ffi.hide), want(&core.hide), "hide ranges mirror core");
        assert_eq!(got(&ffi.dim), want(&core.dim), "dim ranges mirror core");
        ffi
    }

    #[test]
    fn compose_decoration_plan_hides_inline_markers_away_from_caret() {
        // With the caret outside the `**bold**` run (byte 13, in " rest"), both `**` runs are
        // inline emphasis away from the caret → CONCEAL (`hide`), nothing dimmed. `**` markers
        // are the 2-byte runs `[0,2)` and `[6,8)`.
        let plan = assert_mirrors_core("**bold** rest", 13);
        assert_eq!(plan.dim.len(), 0, "no structural prefix / revealed run");
        let hides: Vec<(u64, u64)> = plan.hide.iter().map(|s| (s.start, s.end)).collect();
        assert_eq!(hides, vec![(0, 2), (6, 8)], "both `**` runs concealed");
    }

    #[test]
    fn compose_decoration_plan_reveals_dimmed_marker_under_caret() {
        // Caret inside the bold run (byte 4) reveals its markers: they move from `hide` to
        // `dim` (shown dimmed + editable), never concealed.
        let plan = assert_mirrors_core("**bold** rest", 4);
        assert_eq!(plan.hide.len(), 0, "caret in the run → nothing concealed");
        let dims: Vec<(u64, u64)> = plan.dim.iter().map(|s| (s.start, s.end)).collect();
        assert_eq!(
            dims,
            vec![(0, 2), (6, 8)],
            "both `**` runs revealed (dimmed)"
        );
    }

    #[test]
    fn compose_decoration_plan_dims_structural_prefix_off_caret_line() {
        // A heading prefix on a line the caret is NOT on dims (structural markers are never
        // concealed); the caret is on the second line ("# h\nx", caret on `x`). Exact prefix
        // width comes from core (the mirror assert); semantically it must dim, never hide.
        let plan = assert_mirrors_core("# h\nx", 4);
        assert!(
            !plan.dim.is_empty(),
            "heading prefix dimmed off the caret line"
        );
        assert_eq!(plan.hide.len(), 0, "structural prefix is never concealed");
    }

    #[test]
    fn compose_decoration_plan_empty_for_plain_text() {
        let plan = assert_mirrors_core("just plain text", 0);
        assert!(plan.hide.is_empty() && plan.dim.is_empty());
    }

    #[cfg(feature = "markdown-authoring")]
    #[test]
    fn compose_show_markers_dim_ranges_mirrors_core() {
        // The façade mirrors `fauna_core` faithfully (same byte ranges, usize→u64). "a\n*b*": the
        // two `*` markers are on line 1 → caret on line 0 dims both; caret on line 1 dims none.
        let mirror = |src: &str, caret: u64| -> Vec<(u64, u64)> {
            let core = markdown::compose_show_markers_dim_ranges(src, caret as usize);
            let ffi = compose_show_markers_dim_ranges(src.to_string(), caret);
            let got: Vec<(u64, u64)> = ffi.iter().map(|s| (s.start, s.end)).collect();
            let want: Vec<(u64, u64)> = core
                .iter()
                .map(|s| (s.start as u64, s.end as u64))
                .collect();
            assert_eq!(got, want, "FFI dim ranges mirror core for caret {caret}");
            got
        };
        assert_eq!(mirror("a\n*b*", 0), vec![(2, 3), (4, 5)]);
        assert!(mirror("a\n*b*", 3).is_empty());
    }

    #[cfg(feature = "markdown-authoring")]
    #[test]
    fn wrap_markdown_selection_keeps_trailing_space_outside() {
        // The façade maps the shared wrap through unchanged: the trailing space the
        // double-click selection picked up stays outside the markers (`*italic* `, not
        // `*italic *`), so it no longer collides with an adjacent `**bold**`.
        let w = wrap_markdown_selection("italic ".into(), "*".into(), "*".into(), "text".into());
        assert_eq!(w.replacement, "*italic* ");
        assert_eq!(w.before_core, "*");
        assert_eq!(w.core, "italic");
    }
}
