//! Shared NIP-23 article Markdown parser — the canonical home for the small Markdown
//! subset every app renders for `nostr/article` bodies (`docs/goal/ui/feed.md` §
//! Article). Replaces the drifting per-app parsers (web `markdown.ts` block→HTML,
//! android `MarkdownText.kt` inline→AnnotatedString, windows `MarkdownHelper.cs`
//! inline→segments, and the Linux DM bubble's former bespoke
//! `views/conversations/markdown.rs`) with one definition (priority #1/#2/#4): clients
//! render the same token model natively, instead of each supporting a different
//! Markdown subset.
//!
//! Entry points over one parser:
//! - [`parse_markdown`] → a flat [`MdBlock`] token model the native apps render.
//! - [`render_html`] / [`markdown_to_html`] → the canonical HTML the web app emits
//!   (ports `markdown.ts`; byte-identical for non-nested inline — the only content that
//!   appears in practice). Remote images load (`<img src>`).
//! - [`markdown_to_html_blocked`] → the same HTML but with remote `![]()` images
//!   **blocked** (no `src`) for untrusted inbound mail/DM bodies; [`count_remote_images`]
//!   gates the per-message reveal. See [`RemoteImageMode`].
//!
//! Grammar (the union of every app's subset — this is the canonical superset the
//! Linux DM bubble, web, windows, and android all render): block — ATX headings
//! `# … ####`, unordered lists (`-`/`*`/`+`), ordered lists (`1. … `, renumbered
//! from 1 at render like `<ol>`), `> ` blockquotes, fenced ``` code blocks,
//! blank-line-separated paragraphs; inline — `` `code` ``, `***bold-italic***`,
//! `**bold**`, `*italic*` — and the underscore emphasis forms `___both___` /
//! `__bold__` / `_italic_` (CommonMark's intraword rule applies, so `snake_case`
//! stays literal). The underscore forms are load-bearing for mail: the inbound
//! HTML→markdown converter (`htmd`, in `fauna-conversations`) emits `_…_` for
//! `<em>`/`<i>`, so without them every received HTML email's italics rendered as
//! literal `_underscores_` on every app. Also — `[label](http(s)://…)`,
//! `![alt](http(s)://…)` remote images.

use std::ops::Range;

use serde::{Deserialize, Serialize};

/// One inline run within a line: plain text plus its active style(s). `code` and
/// `link`/`image_url` are mutually exclusive with everything else; `bold` and `italic`
/// may both be set on the same run (a `***bold-italic***` span). `link` carries the
/// href, `image_url` the source of a `![alt](url)` remote image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MdSpan {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    /// Link href when this span is a `[label](url)` link; empty otherwise.
    pub link: String,
    /// Image source url when this span is a `![alt](url)` **remote** image
    /// (`http(s)` only, mirroring [`parse_link`]); empty otherwise. `text` holds
    /// the alt text. The url is **never fetched by the parser** — it is left for
    /// each app's renderer to display blocked-by-default (the privacy posture
    /// for untrusted inbound content) and reveal on a per-message opt-in. See
    /// [`RemoteImageMode`].
    pub image_url: String,
}

impl MdSpan {
    fn plain(text: String) -> Self {
        MdSpan {
            text,
            ..Default::default()
        }
    }

    /// A `![alt](url)` remote-image span: `alt` text, `url` source (un-fetched).
    fn image(alt: String, url: String) -> Self {
        MdSpan {
            text: alt,
            image_url: url,
            ..Default::default()
        }
    }

    /// True when this span is a remote `![alt](url)` image (vs. text/link/style).
    pub fn is_image(&self) -> bool {
        !self.image_url.is_empty()
    }
}

/// A single line of inline content (a heading's text, a paragraph, or one list item).
///
/// `depth` and `task` are list-item annotations (render-model.md § D7b), **additive and
/// default-zero** so a non-list line and a non-nested, non-task list item are byte-identical
/// to before (the shipped `MdBlock`/html-mail path renders `spans` and ignores both fields;
/// `FfiMdLine` does not mirror them, so no client face changes). `depth` is the nesting level
/// (every 2 leading spaces, or one tab, = one level); `task` is `Some(checked)` for a GFM
/// task item (`- [ ]`/`- [x]`) else `None`. The typed `markdown_to_document` reads them to fold
/// the flat list into nested `ListBlock`/`TaskList` blocks; `spans` keeps the `[ ]`/`[x]` marker
/// verbatim (so html-mail is unchanged) and the typed projection strips it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdLine {
    pub spans: Vec<MdSpan>,
    #[serde(default)]
    pub depth: u8,
    #[serde(default)]
    pub task: Option<bool>,
}

/// A block-level node. `kind` is `"heading"` | `"paragraph"` | `"list"` |
/// `"ordered_list"` | `"blockquote"` | `"code_block"`. `level` is the heading level
/// (1–4; 0 otherwise). `code` is the raw fenced-code text (empty for non-code blocks).
/// `lines` holds the inline content: one line for heading/paragraph/blockquote, one per
/// item for a list / ordered list (an ordered list renumbers from 1 at render, like
/// `<ol>`), empty for a code block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdBlock {
    pub kind: String,
    pub level: u8,
    pub code: String,
    pub lines: Vec<MdLine>,
}

/// A visual decoration over a byte range of the **raw source** string — the model the
/// compose field uses for inline markdown styling (the Obsidian/Typora "live preview").
/// `start`/`end` are byte indices into the `src` passed to [`decoration_map`]; the source
/// is **not** `\r\n`-normalized, so offsets map straight onto the editor's live buffer.
/// `level` carries the heading level (1–4) when `kind` is [`MdDecorationKind::Heading`],
/// 0 otherwise — mirroring [`MdBlock`]'s `kind` + `level` shape so a future reader sees
/// the two models the same way (rather than a `Heading(u8)` payload variant that every
/// binding generator would have to special-case).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdDecoration {
    pub start: usize,
    pub end: usize,
    pub kind: MdDecorationKind,
    pub level: u8,
}

/// The kind of an [`MdDecoration`]. `Marker` is a syntax marker (`*`, `**`, `` ` ``, `#`,
/// `>`, `-`/`1.`, `[`, `](url)`, ```` ``` ````) the client dims/conceals and reveals near
/// the caret; the rest are the styled-content kinds applied to the unwrapped text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MdDecorationKind {
    Marker,
    Bold,
    Italic,
    BoldItalic,
    Code,
    Link,
    Image,
    Heading,
    Blockquote,
    ListMarker,
}

impl MdDecorationKind {
    /// Stable snake_case name — the cross-app wire string for the UniFFI
    /// (`FfiMdDecoration.kind`) and wasm (`decorationMap`) faces. Matches the serde
    /// `rename_all = "snake_case"` form so every app sees the same token.
    pub fn as_str(self) -> &'static str {
        match self {
            MdDecorationKind::Marker => "marker",
            MdDecorationKind::Bold => "bold",
            MdDecorationKind::Italic => "italic",
            MdDecorationKind::BoldItalic => "bold_italic",
            MdDecorationKind::Code => "code",
            MdDecorationKind::Link => "link",
            MdDecorationKind::Image => "image",
            MdDecorationKind::Heading => "heading",
            MdDecorationKind::Blockquote => "blockquote",
            MdDecorationKind::ListMarker => "list_marker",
        }
    }
}

impl MdDecoration {
    fn new(start: usize, end: usize, kind: MdDecorationKind) -> Self {
        MdDecoration {
            start,
            end,
            kind,
            level: 0,
        }
    }

    fn heading(start: usize, end: usize, level: u8) -> Self {
        MdDecoration {
            start,
            end,
            kind: MdDecorationKind::Heading,
            level,
        }
    }
}

const KIND_HEADING: &str = "heading";
const KIND_PARAGRAPH: &str = "paragraph";
const KIND_LIST: &str = "list";
const KIND_ORDERED_LIST: &str = "ordered_list";
const KIND_BLOCKQUOTE: &str = "blockquote";
const KIND_CODE_BLOCK: &str = "code_block";

/// Parse a Markdown string into the flat block/line/span token model.
pub fn parse_markdown(md: &str) -> Vec<MdBlock> {
    let normalized = md.replace("\r\n", "\n");
    let lines: Vec<&str> = normalized.split('\n').collect();

    let mut blocks: Vec<MdBlock> = Vec::new();
    let mut in_code = false;
    let mut code_lines: Vec<String> = Vec::new();
    // List items carry their nesting `depth` and (unordered only) GFM `task` state so the typed
    // `markdown_to_document` can fold the flat run into nested `ListBlock`/`TaskList` blocks
    // (render-model.md § D7b). The shipped html-mail path (`render_html`) ignores both.
    let mut list_items: Vec<(u8, Option<bool>, String)> = Vec::new();
    let mut ordered_items: Vec<(u8, String)> = Vec::new();
    let mut para_lines: Vec<String> = Vec::new();

    fn flush_para(para_lines: &mut Vec<String>, blocks: &mut Vec<MdBlock>) {
        if para_lines.is_empty() {
            return;
        }
        let text = para_lines.join(" ");
        let text = text.trim();
        if !text.is_empty() {
            blocks.push(MdBlock {
                kind: KIND_PARAGRAPH.into(),
                level: 0,
                code: String::new(),
                lines: vec![MdLine {
                    spans: parse_inline(text),
                    ..Default::default()
                }],
            });
        }
        para_lines.clear();
    }

    /// Flush an accumulated unordered list into one block. Each item keeps its `depth`
    /// (nesting level) and `task` (GFM checkbox state) so `markdown_to_document` can fold the
    /// flat run into nested `ListBlock`/`TaskList` blocks. The item text (`spans`) is verbatim
    /// — including any `[ ]`/`[x]` marker — so the shipped html-mail path is byte-identical.
    fn flush_list(list_items: &mut Vec<(u8, Option<bool>, String)>, blocks: &mut Vec<MdBlock>) {
        if list_items.is_empty() {
            return;
        }
        let lines = list_items
            .iter()
            .map(|(depth, task, li)| MdLine {
                spans: parse_inline(li),
                depth: *depth,
                task: *task,
            })
            .collect();
        blocks.push(MdBlock {
            kind: KIND_LIST.into(),
            level: 0,
            code: String::new(),
            lines,
        });
        list_items.clear();
    }

    /// Flush an accumulated ordered list into one block. An ordered list carries no item
    /// numbers — it renumbers from 1 at render (matching `<ol>`) — and no task state; only
    /// `depth` (nesting level) is captured.
    fn flush_ordered(ordered_items: &mut Vec<(u8, String)>, blocks: &mut Vec<MdBlock>) {
        if ordered_items.is_empty() {
            return;
        }
        let lines = ordered_items
            .iter()
            .map(|(depth, li)| MdLine {
                spans: parse_inline(li),
                depth: *depth,
                task: None,
            })
            .collect();
        blocks.push(MdBlock {
            kind: KIND_ORDERED_LIST.into(),
            level: 0,
            code: String::new(),
            lines,
        });
        ordered_items.clear();
    }

    for line in &lines {
        // Fenced code block toggle.
        if line.trim_start().starts_with("```") {
            if !in_code {
                flush_para(&mut para_lines, &mut blocks);
                flush_list(&mut list_items, &mut blocks);
                flush_ordered(&mut ordered_items, &mut blocks);
                in_code = true;
                code_lines.clear();
            } else {
                blocks.push(MdBlock {
                    kind: KIND_CODE_BLOCK.into(),
                    level: 0,
                    code: code_lines.join("\n"),
                    lines: Vec::new(),
                });
                code_lines.clear();
                in_code = false;
            }
            continue;
        }

        if in_code {
            code_lines.push((*line).to_string());
            continue;
        }

        // ATX heading: 1–4 '#' then whitespace then content.
        if let Some((level, content)) = parse_heading(line) {
            flush_para(&mut para_lines, &mut blocks);
            flush_list(&mut list_items, &mut blocks);
            flush_ordered(&mut ordered_items, &mut blocks);
            blocks.push(MdBlock {
                kind: KIND_HEADING.into(),
                level,
                code: String::new(),
                lines: vec![MdLine {
                    spans: parse_inline(content),
                    ..Default::default()
                }],
            });
            continue;
        }

        // Blockquote: `> ` then content — one block per quoted line (mirrors the
        // heading/paragraph single-line shape).
        if let Some(content) = line.strip_prefix("> ") {
            flush_para(&mut para_lines, &mut blocks);
            flush_list(&mut list_items, &mut blocks);
            flush_ordered(&mut ordered_items, &mut blocks);
            blocks.push(MdBlock {
                kind: KIND_BLOCKQUOTE.into(),
                level: 0,
                code: String::new(),
                lines: vec![MdLine {
                    spans: parse_inline(content),
                    ..Default::default()
                }],
            });
            continue;
        }

        // Unordered list item: optional indent, then -, *, or + then whitespace then content
        // (optionally a `[ ]`/`[x]` task marker). Ends any open ordered list before it.
        if let Some((depth, task, content)) = parse_list_item(line) {
            flush_para(&mut para_lines, &mut blocks);
            flush_ordered(&mut ordered_items, &mut blocks);
            list_items.push((depth, task, content.to_string()));
            continue;
        }

        // Ordered list item: optional indent, then digit(s) + `. ` then content. Ends any
        // open unordered list before it.
        if let Some((depth, content)) = parse_ordered_item(line) {
            flush_para(&mut para_lines, &mut blocks);
            flush_list(&mut list_items, &mut blocks);
            ordered_items.push((depth, content.to_string()));
            continue;
        }

        // Blank line: flush paragraph and any open list.
        if line.trim().is_empty() {
            flush_para(&mut para_lines, &mut blocks);
            flush_list(&mut list_items, &mut blocks);
            flush_ordered(&mut ordered_items, &mut blocks);
            continue;
        }

        // A non-list line ends any open list before this paragraph line.
        if !list_items.is_empty() {
            flush_list(&mut list_items, &mut blocks);
        }
        if !ordered_items.is_empty() {
            flush_ordered(&mut ordered_items, &mut blocks);
        }
        para_lines.push((*line).to_string());
    }

    if in_code && !code_lines.is_empty() {
        blocks.push(MdBlock {
            kind: KIND_CODE_BLOCK.into(),
            level: 0,
            code: code_lines.join("\n"),
            lines: Vec::new(),
        });
    }
    flush_para(&mut para_lines, &mut blocks);
    flush_list(&mut list_items, &mut blocks);
    flush_ordered(&mut ordered_items, &mut blocks);

    blocks
}

/// `# … ####` → `(level, content)`; requires whitespace and non-empty content after the
/// hashes (mirrors web's `^(#{1,4})\s+(.+)$`).
fn parse_heading(line: &str) -> Option<(u8, &str)> {
    let hashes = line.bytes().take_while(|&b| b == b'#').count();
    if hashes == 0 || hashes > 4 {
        return None;
    }
    let rest = &line[hashes..];
    let trimmed = rest.trim_start_matches([' ', '\t']);
    // Require at least one whitespace char between the hashes and the content.
    if trimmed.len() == rest.len() || trimmed.is_empty() {
        return None;
    }
    Some((hashes as u8, trimmed))
}

/// The nesting depth of a list item from its leading indentation (render-model.md § D7b):
/// every two spaces, or one tab, counts as one level. A non-indented item is depth 0 — so a
/// non-nested list is byte-identical to before. The fold in `markdown_to_document` only uses
/// **relative** depth, so this normalization need not match a renderer exactly; it just has to
/// be stable (serialize depth N → 2N spaces → reparse → N).
fn list_depth(indent: &str) -> u8 {
    let units: usize = indent.chars().map(|c| if c == '\t' { 2 } else { 1 }).sum();
    u8::try_from(units / 2).unwrap_or(u8::MAX)
}

/// `[indent]N. item` (one or more ASCII digits, then `. `) → `(depth, item)`. The literal
/// number is **discarded** — ordered lists renumber from 1 at render (matching `<ol>`). The
/// digits must immediately follow the indent (anything else before the first `. ` → not an
/// ordered item, e.g. `Section 2. Details`).
fn parse_ordered_item(line: &str) -> Option<(u8, &str)> {
    let indent_len = line.len() - line.trim_start_matches([' ', '\t']).len();
    let body = &line[indent_len..];
    let dot_pos = body.find(". ")?;
    let num = &body[..dot_pos];
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((list_depth(&line[..indent_len]), &body[dot_pos + 2..]))
}

/// `[indent]- item` / `* item` / `+ item` → `(depth, task, content)`, with optional leading
/// indentation and GFM task syntax (render-model.md § D7b; mirrors web's `^(\s*)[-*+]\s+(.+)$`
/// plus task recognition). `content` is **verbatim, keeping any `[ ]`/`[x]` marker**, so the
/// shipped html-mail path renders it byte-identically; the typed `markdown_to_document` strips
/// the marker using `task`. `task` is `Some(checked)` for a task item, else `None`.
fn parse_list_item(line: &str) -> Option<(u8, Option<bool>, &str)> {
    let indent_len = line.len() - line.trim_start_matches([' ', '\t']).len();
    let body = &line[indent_len..];
    let first = body.chars().next()?;
    if first != '-' && first != '*' && first != '+' {
        return None;
    }
    let rest = &body[first.len_utf8()..];
    let trimmed = rest.trim_start_matches([' ', '\t']);
    if trimmed.len() == rest.len() || trimmed.is_empty() {
        return None;
    }
    Some((
        list_depth(&line[..indent_len]),
        task_state(trimmed),
        trimmed,
    ))
}

/// Recognize a GFM task marker at the **start** of a list item's content: `[ ] ` → unchecked,
/// `[x] ` / `[X] ` → checked (case-insensitive). Requires whitespace after the `]` and
/// non-empty content following (a bare `[ ]`, or `[link](url)`, is not a task). Returns the
/// checked state, or `None` when the content is a plain bullet.
fn task_state(content: &str) -> Option<bool> {
    let rest = content.strip_prefix('[')?;
    let mut chars = rest.chars();
    let mark = chars.next()?;
    if chars.next()? != ']' {
        return None;
    }
    let checked = match mark {
        ' ' => false,
        'x' | 'X' => true,
        _ => return None,
    };
    // `mark` and `]` are each one byte, so the content after `]` starts at byte 2 of `rest`.
    let after = &rest[2..];
    let body = after.trim_start_matches([' ', '\t']);
    if body.len() == after.len() || body.is_empty() {
        return None;
    }
    Some(checked)
}

/// One inline run as scanned from the source, carrying the byte ranges of its content
/// and its syntax markers **within the slice passed to [`scan_inline`]**. This is the
/// shared substrate both [`parse_inline`] (the render token model, which drops the ranges)
/// and [`decoration_map`] (compose styling, which keeps them) consume — so the editor's
/// inline-styling preview and the rendered/sent message can never disagree on which source
/// bytes are styled.
struct InlineToken {
    kind: InlineKind,
    /// Byte range of the unwrapped content (styled text / link label / image alt).
    content: Range<usize>,
    /// Unwrapped text: content for plain & styled runs, the label for a link, the alt for
    /// an image.
    text: String,
    /// Link href / image source; empty otherwise.
    url: String,
    /// Byte ranges of the syntax markers (`*`, `` ` ``, `[`, `](url)`, …). Empty for a
    /// plain run; the first entry is always the opener (before `content`), the rest follow
    /// `content`.
    markers: Vec<Range<usize>>,
}

#[derive(Clone, Copy)]
enum InlineKind {
    Plain,
    Bold,
    Italic,
    BoldItalic,
    Code,
    Link,
    Image,
}

impl InlineToken {
    /// Project to the render [`MdSpan`] (drops the source ranges).
    fn into_span(self) -> MdSpan {
        match self.kind {
            InlineKind::Plain => MdSpan::plain(self.text),
            InlineKind::Code => MdSpan {
                text: self.text,
                code: true,
                ..Default::default()
            },
            InlineKind::Bold => MdSpan {
                text: self.text,
                bold: true,
                ..Default::default()
            },
            InlineKind::Italic => MdSpan {
                text: self.text,
                italic: true,
                ..Default::default()
            },
            InlineKind::BoldItalic => MdSpan {
                text: self.text,
                bold: true,
                italic: true,
                ..Default::default()
            },
            InlineKind::Link => MdSpan {
                text: self.text,
                link: self.url,
                ..Default::default()
            },
            InlineKind::Image => MdSpan::image(self.text, self.url),
        }
    }
}

/// Flush the accumulated plain run (`[normal_start, end)` in source bytes) as a token.
fn flush_plain(
    normal: &mut String,
    normal_start: usize,
    end: usize,
    tokens: &mut Vec<InlineToken>,
) {
    if !normal.is_empty() {
        tokens.push(InlineToken {
            kind: InlineKind::Plain,
            content: normal_start..end,
            text: std::mem::take(normal),
            url: String::new(),
            markers: Vec::new(),
        });
    }
}

/// The one inline scanner (see [`InlineToken`]). Same grammar and precedence as the former
/// `parse_inline` body — code before bold-italic before bold before link before italic,
/// then the underscore family — but it records **source byte ranges** so both the render
/// token model and the compose decoration map derive from a single tokenization. Asterisk
/// emphasis is unrestricted; underscore emphasis follows CommonMark's intraword rule (an
/// opening `_` after, or a closing `_` before, an alphanumeric is literal text, so
/// `snake_case` stays literal — required because the inbound HTML→markdown converter emits
/// `_…_` for `<em>`/`<i>`).
fn scan_inline(text: &str) -> Vec<InlineToken> {
    let chars: Vec<char> = text.chars().collect();
    // Byte offset of each char's start; `byte_at[chars.len()] == text.len()`. Lets the
    // char-index matching below emit byte ranges into the original `text` (UTF-8 safe).
    let mut byte_at: Vec<usize> = Vec::with_capacity(chars.len() + 1);
    {
        let mut b = 0;
        for c in &chars {
            byte_at.push(b);
            b += c.len_utf8();
        }
        byte_at.push(b);
    }

    let mut tokens: Vec<InlineToken> = Vec::new();
    let mut normal = String::new();
    let mut normal_start = 0usize;
    let mut i = 0;

    while i < chars.len() {
        // Inline code: `code`
        if chars[i] == '`'
            && let Some(end) = find_char(&chars, i + 1, '`')
        {
            flush_plain(&mut normal, normal_start, byte_at[i], &mut tokens);
            tokens.push(InlineToken {
                kind: InlineKind::Code,
                content: byte_at[i + 1]..byte_at[end],
                text: chars[i + 1..end].iter().collect(),
                url: String::new(),
                markers: vec![byte_at[i]..byte_at[i + 1], byte_at[end]..byte_at[end + 1]],
            });
            i = end + 1;
            continue;
        }
        // Image: ![alt](http(s)://…) — checked before the bare `[` link branch so the
        // leading `!` binds to the image, not a literal `!` + link. The `![` opener and
        // the `](url)` tail are the two marker ranges; `content` is the alt text.
        if chars[i] == '!'
            && i + 1 < chars.len()
            && chars[i + 1] == '['
            && let Some(link) = parse_link(&chars, i + 1)
        {
            flush_plain(&mut normal, normal_start, byte_at[i], &mut tokens);
            let next = i + 1 + link.consumed;
            tokens.push(InlineToken {
                kind: InlineKind::Image,
                content: byte_at[i + 2]..byte_at[link.close_bracket],
                text: link.span.text,
                url: link.span.link,
                markers: vec![
                    byte_at[i]..byte_at[i + 2],
                    byte_at[link.close_bracket]..byte_at[next],
                ],
            });
            i = next;
            continue;
        }
        // Bold-italic: ***text*** — checked before the `**` bold branch so the leading
        // `**` doesn't bind as plain bold. Sets BOTH bold and italic.
        if chars[i] == '*'
            && i + 2 < chars.len()
            && chars[i + 1] == '*'
            && chars[i + 2] == '*'
            && let Some(end) = find_seq(&chars, i + 3, &['*', '*', '*'])
            && end > i + 3
        {
            flush_plain(&mut normal, normal_start, byte_at[i], &mut tokens);
            tokens.push(InlineToken {
                kind: InlineKind::BoldItalic,
                content: byte_at[i + 3]..byte_at[end],
                text: chars[i + 3..end].iter().collect(),
                url: String::new(),
                markers: vec![byte_at[i]..byte_at[i + 3], byte_at[end]..byte_at[end + 3]],
            });
            i = end + 3;
            continue;
        }
        // Bold: **text**
        if chars[i] == '*'
            && i + 1 < chars.len()
            && chars[i + 1] == '*'
            && let Some(end) = find_seq(&chars, i + 2, &['*', '*'])
        {
            flush_plain(&mut normal, normal_start, byte_at[i], &mut tokens);
            tokens.push(InlineToken {
                kind: InlineKind::Bold,
                content: byte_at[i + 2]..byte_at[end],
                text: chars[i + 2..end].iter().collect(),
                url: String::new(),
                markers: vec![byte_at[i]..byte_at[i + 2], byte_at[end]..byte_at[end + 2]],
            });
            i = end + 2;
            continue;
        }
        // Link: [label](http(s)://…) — the `[` opener and `](url)` tail are the markers.
        if chars[i] == '['
            && let Some(link) = parse_link(&chars, i)
        {
            flush_plain(&mut normal, normal_start, byte_at[i], &mut tokens);
            let next = i + link.consumed;
            tokens.push(InlineToken {
                kind: InlineKind::Link,
                content: byte_at[i + 1]..byte_at[link.close_bracket],
                text: link.span.text,
                url: link.span.link,
                markers: vec![
                    byte_at[i]..byte_at[i + 1],
                    byte_at[link.close_bracket]..byte_at[next],
                ],
            });
            i = next;
            continue;
        }
        // Italic: *text* (single star, not part of **)
        if chars[i] == '*'
            && let Some(end) = find_char(&chars, i + 1, '*')
            && end > i + 1
            && !(end + 1 < chars.len() && chars[end + 1] == '*')
        {
            flush_plain(&mut normal, normal_start, byte_at[i], &mut tokens);
            tokens.push(InlineToken {
                kind: InlineKind::Italic,
                content: byte_at[i + 1]..byte_at[end],
                text: chars[i + 1..end].iter().collect(),
                url: String::new(),
                markers: vec![byte_at[i]..byte_at[i + 1], byte_at[end]..byte_at[end + 1]],
            });
            i = end + 1;
            continue;
        }
        // Bold-italic: ___text___ (underscore form) — longest-first; intraword rule applies.
        if chars[i] == '_'
            && underscore_opens(&chars, i)
            && i + 2 < chars.len()
            && chars[i + 1] == '_'
            && chars[i + 2] == '_'
            && let Some(end) = find_seq(&chars, i + 3, &['_', '_', '_'])
            && end > i + 3
            && underscore_closes(&chars, end + 3)
        {
            flush_plain(&mut normal, normal_start, byte_at[i], &mut tokens);
            tokens.push(InlineToken {
                kind: InlineKind::BoldItalic,
                content: byte_at[i + 3]..byte_at[end],
                text: chars[i + 3..end].iter().collect(),
                url: String::new(),
                markers: vec![byte_at[i]..byte_at[i + 3], byte_at[end]..byte_at[end + 3]],
            });
            i = end + 3;
            continue;
        }
        // Bold: __text__ (underscore form)
        if chars[i] == '_'
            && underscore_opens(&chars, i)
            && i + 1 < chars.len()
            && chars[i + 1] == '_'
            && let Some(end) = find_seq(&chars, i + 2, &['_', '_'])
            && end > i + 2
            && underscore_closes(&chars, end + 2)
        {
            flush_plain(&mut normal, normal_start, byte_at[i], &mut tokens);
            tokens.push(InlineToken {
                kind: InlineKind::Bold,
                content: byte_at[i + 2]..byte_at[end],
                text: chars[i + 2..end].iter().collect(),
                url: String::new(),
                markers: vec![byte_at[i]..byte_at[i + 2], byte_at[end]..byte_at[end + 2]],
            });
            i = end + 2;
            continue;
        }
        // Italic: _text_ (single underscore, not part of __, not intraword)
        if chars[i] == '_'
            && underscore_opens(&chars, i)
            && let Some(end) = find_char(&chars, i + 1, '_')
            && end > i + 1
            && !(end + 1 < chars.len() && chars[end + 1] == '_')
            && underscore_closes(&chars, end + 1)
        {
            flush_plain(&mut normal, normal_start, byte_at[i], &mut tokens);
            tokens.push(InlineToken {
                kind: InlineKind::Italic,
                content: byte_at[i + 1]..byte_at[end],
                text: chars[i + 1..end].iter().collect(),
                url: String::new(),
                markers: vec![byte_at[i]..byte_at[i + 1], byte_at[end]..byte_at[end + 1]],
            });
            i = end + 1;
            continue;
        }
        if normal.is_empty() {
            normal_start = byte_at[i];
        }
        normal.push(chars[i]);
        i += 1;
    }
    flush_plain(&mut normal, normal_start, byte_at[chars.len()], &mut tokens);
    tokens
}

/// Parse inline markup into a flat span list (see the module grammar). Thin projection of
/// the shared [`scan_inline`] tokenizer to the render [`MdSpan`] model.
pub fn parse_inline(text: &str) -> Vec<MdSpan> {
    scan_inline(text)
        .into_iter()
        .map(InlineToken::into_span)
        .collect()
}

/// Build the inline-styling decoration map for a compose buffer: the styled-content and
/// marker byte ranges over the **raw** `src` (NOT `\r\n`-normalized — offsets map straight
/// onto the editor's live string). Drives the compose field's live markdown preview; the
/// same [`scan_inline`] tokenizer feeds [`parse_markdown`], so the preview and the
/// sent/rendered message agree on what is styled. Decorations are emitted in ascending
/// `start` order (a styled run's opening marker, then its content style, then its trailing
/// markers; block markers/styles before the line's inline decorations).
///
/// Block structure is classified **per source line** (same order as [`parse_markdown`]:
/// fenced code, heading, blockquote, unordered list, ordered list, else paragraph), so an
/// emphasis run spanning a hard line break is left literal — the documented limitation in
/// `docs/goal/ui/conversations.md` § Compose-field inline markdown styling.
pub fn decoration_map(src: &str) -> Vec<MdDecoration> {
    let bytes = src.as_bytes();
    let mut out: Vec<MdDecoration> = Vec::new();
    let mut in_code = false;
    let mut pos = 0usize;
    loop {
        let nl = src[pos..].find('\n').map(|o| pos + o);
        let line_end = nl.unwrap_or(src.len());
        // The trailing '\r' of a CRLF is not content, but we do NOT shift other offsets.
        let mut content_end = line_end;
        if content_end > pos && bytes[content_end - 1] == b'\r' {
            content_end -= 1;
        }
        decorate_line(&src[pos..content_end], pos, &mut in_code, &mut out);
        match nl {
            Some(n) => pos = n + 1,
            None => break,
        }
    }
    out
}

/// Byte offset of `sub` within `parent`; `sub` must be a sub-slice of `parent` (as
/// returned by `parse_heading` / `parse_list_item` / `parse_ordered_item`).
fn offset_in(parent: &str, sub: &str) -> usize {
    sub.as_ptr() as usize - parent.as_ptr() as usize
}

/// Emit the decorations for one source line. `abs` is the line's byte offset in the source;
/// `in_code` carries the fenced-code-block toggle across lines.
fn decorate_line(line: &str, abs: usize, in_code: &mut bool, out: &mut Vec<MdDecoration>) {
    let trimmed = line.trim_start();
    if trimmed.starts_with("```") {
        let ws = line.len() - trimmed.len();
        out.push(MdDecoration::new(
            abs + ws,
            abs + line.len(),
            MdDecorationKind::Marker,
        ));
        *in_code = !*in_code;
        return;
    }
    if *in_code {
        if !line.is_empty() {
            out.push(MdDecoration::new(
                abs,
                abs + line.len(),
                MdDecorationKind::Code,
            ));
        }
        return;
    }
    if let Some((level, content)) = parse_heading(line) {
        let coff = offset_in(line, content);
        out.push(MdDecoration::new(abs, abs + coff, MdDecorationKind::Marker));
        out.push(MdDecoration::heading(abs + coff, abs + line.len(), level));
        push_inline_decorations(content, abs + coff, out);
        return;
    }
    if let Some(content) = line.strip_prefix("> ") {
        out.push(MdDecoration::new(abs, abs + 2, MdDecorationKind::Marker));
        out.push(MdDecoration::new(
            abs + 2,
            abs + line.len(),
            MdDecorationKind::Blockquote,
        ));
        push_inline_decorations(content, abs + 2, out);
        return;
    }
    if let Some((_, _, content)) = parse_list_item(line) {
        let coff = offset_in(line, content);
        out.push(MdDecoration::new(
            abs,
            abs + coff,
            MdDecorationKind::ListMarker,
        ));
        push_inline_decorations(content, abs + coff, out);
        return;
    }
    if let Some((_, content)) = parse_ordered_item(line) {
        let coff = offset_in(line, content);
        out.push(MdDecoration::new(
            abs,
            abs + coff,
            MdDecorationKind::ListMarker,
        ));
        push_inline_decorations(content, abs + coff, out);
        return;
    }
    push_inline_decorations(line, abs, out);
}

/// Run the shared inline scanner over `content` and append its decorations, rebased by
/// `base` (the content's byte offset in the source). Plain runs emit nothing; each styled
/// run emits its opening marker, the content style, then its trailing markers — already in
/// ascending `start` order.
fn push_inline_decorations(content: &str, base: usize, out: &mut Vec<MdDecoration>) {
    for tok in scan_inline(content) {
        let kind = match tok.kind {
            InlineKind::Plain => continue,
            InlineKind::Bold => MdDecorationKind::Bold,
            InlineKind::Italic => MdDecorationKind::Italic,
            InlineKind::BoldItalic => MdDecorationKind::BoldItalic,
            InlineKind::Code => MdDecorationKind::Code,
            InlineKind::Link => MdDecorationKind::Link,
            InlineKind::Image => MdDecorationKind::Image,
        };
        let mut markers = tok.markers.into_iter();
        if let Some(open) = markers.next() {
            out.push(MdDecoration::new(
                base + open.start,
                base + open.end,
                MdDecorationKind::Marker,
            ));
        }
        out.push(MdDecoration::new(
            base + tok.content.start,
            base + tok.content.end,
            kind,
        ));
        for close in markers {
            out.push(MdDecoration::new(
                base + close.start,
                base + close.end,
                MdDecorationKind::Marker,
            ));
        }
    }
}

/// A byte range of inline-emphasis markers a markers-hidden (Notes-fidelity) editor should
/// **reveal** because the caret sits within their styled run, so the raw markdown markers
/// (`**`, `*`, `` ` ``, `[`/`](url)`) become ordinary editable text. Offsets are UTF-8 byte
/// indices into the `src` passed to [`inline_reveal_ranges`] (the same offset space as
/// [`decoration_map`]); the caller maps them to its native unit (UTF-16 for JS/Swift/etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevealSpan {
    pub start: usize,
    pub end: usize,
}

/// The shared **caret-edge reveal policy** for a markers-hidden (Notes-fidelity) editor:
/// which inline-emphasis marker byte-ranges to reveal for a caret at byte offset `caret`.
///
/// A markers-never-shown editor conceals every marker [`decoration_map`] returns; this
/// function decides which markers to un-hide so they can be edited — one definition so web
/// (CodeMirror) and every native app agree on *which* markers reveal at a given caret
/// (priorities #1/#2), instead of each re-deriving the rule. The conversations editor reveals
/// whole-line (`apps/fauna-web/src/lib/markdown-decorations.ts`); Notes reveals **per run** —
/// only the run the caret is within. A run reveals when `caret` sits in `[run_start, run_end]`
/// **inclusive** (`run_start` = its opening marker's start, `run_end` = its closing marker's
/// end), i.e. inside the run or at either edge.
///
/// **Structural** block markers (heading `#`, list `-`/`1.`, quote `>`, fenced code) are
/// never returned — they are block chrome the editor draws from typed block state, never
/// editable inline source. Plain text and structural-only lines yield an empty set. The
/// returned spans are the marker bytes to reveal (the run's content is always styled, hidden
/// or not), in ascending order; `src` is not `\r\n`-normalized, matching [`decoration_map`].
pub fn inline_reveal_ranges(src: &str, caret: usize) -> Vec<RevealSpan> {
    let bytes = src.as_bytes();
    let mut out: Vec<RevealSpan> = Vec::new();
    let mut in_code = false;
    let mut pos = 0usize;
    loop {
        let nl = src[pos..].find('\n').map(|o| pos + o);
        let line_end = nl.unwrap_or(src.len());
        let mut content_end = line_end;
        if content_end > pos && bytes[content_end - 1] == b'\r' {
            content_end -= 1;
        }
        reveal_line(&src[pos..content_end], pos, caret, &mut in_code, &mut out);
        match nl {
            Some(n) => pos = n + 1,
            None => break,
        }
    }
    out
}

/// Append the inline-marker reveal ranges for one source line (see [`inline_reveal_ranges`]):
/// strip the structural prefix (heading/quote/list — never revealed), then reveal any inline
/// run within the content the caret is inside. `abs` is the line's byte offset in the source;
/// `in_code` carries the fenced-code toggle across lines (fenced content has nothing to reveal).
fn reveal_line(
    line: &str,
    abs: usize,
    caret: usize,
    in_code: &mut bool,
    out: &mut Vec<RevealSpan>,
) {
    let trimmed = line.trim_start();
    if trimmed.starts_with("```") {
        *in_code = !*in_code; // a code fence is structural — never revealed
        return;
    }
    if *in_code {
        return;
    }
    // The inline content begins after any structural prefix (mirrors `decorate_line`).
    let (content, base) = if let Some((_level, c)) = parse_heading(line) {
        (c, abs + offset_in(line, c))
    } else if let Some(c) = line.strip_prefix("> ") {
        (c, abs + 2)
    } else if let Some((_, _, c)) = parse_list_item(line) {
        (c, abs + offset_in(line, c))
    } else if let Some((_, c)) = parse_ordered_item(line) {
        (c, abs + offset_in(line, c))
    } else {
        (line, abs)
    };
    for tok in scan_inline(content) {
        // `markers` is empty for a plain run; first = opener (before content), last = the
        // final closer (after content) — so the run spans `[first.start, last.end)`.
        let (Some(first), Some(last)) = (tok.markers.first(), tok.markers.last()) else {
            continue;
        };
        let run_start = base + first.start;
        let run_end = base + last.end;
        if caret >= run_start && caret <= run_end {
            for m in &tok.markers {
                out.push(RevealSpan {
                    start: base + m.start,
                    end: base + m.end,
                });
            }
        }
    }
}

/// The compose-field **marker treatment** for a hide-by-default editor — one definition every
/// app shares (design tracked internally).
/// Byte ranges into the `src` passed to [`compose_decoration_plan`] (same space as
/// [`decoration_map`]); the caller maps to its native unit (UTF-16 for web/CM, chars for GTK).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComposeMarkerPlan {
    /// Inline emphasis markers (`*`/`**`/`` ` ``/`[]()`) to CONCEAL (atomic hide).
    pub hide: Vec<RevealSpan>,
    /// Markers to show DIMMED: structural prefixes (heading/quote/list) off the caret's line,
    /// plus inline emphasis markers revealed because the caret is in their run.
    pub dim: Vec<RevealSpan>,
}

/// One source line's structural info for the compose marker classifier.
struct LinePrefix {
    /// Byte offset of the line's first char.
    start: usize,
    /// Byte offset of the line's end (before its `\n`, or buffer end).
    end: usize,
    /// Byte offset where the structural prefix ends (`= start` when the line has none).
    prefix_end: usize,
    /// A fenced-code line (opening/closing fence or verbatim body) — markers left untouched.
    is_code: bool,
}

/// Per-line structural prefixes, folded exactly as [`crate::notes::parse_note`] maps lines to
/// blocks (a fenced ` ``` ` region → one `Code` block spanning its lines). A non-code line's
/// prefix is everything before the block's `text` (`line.len() - block.text.len()` bytes — the
/// `text` is the line's suffix by construction), so the split is never re-derived here.
fn note_line_prefixes(src: &str) -> Vec<LinePrefix> {
    let blocks = crate::notes::parse_note(src).blocks;
    let lines: Vec<&str> = src.split('\n').collect();
    let mut starts = Vec::with_capacity(lines.len());
    {
        let mut off = 0usize;
        for line in &lines {
            starts.push(off);
            off += line.len() + 1; // + '\n'
        }
    }
    let mk = |i: usize, prefix_end: usize, is_code: bool| LinePrefix {
        start: starts[i],
        end: starts[i] + lines[i].len(),
        prefix_end,
        is_code,
    };
    let mut out = Vec::with_capacity(lines.len());
    let mut bi = 0usize;
    let mut li = 0usize;
    while li < lines.len() {
        match blocks.get(bi) {
            Some(block) if block.kind == crate::notes::BlockKind::Code => {
                out.push(mk(li, starts[li], true)); // opening fence
                li += 1;
                while li < lines.len() && lines[li].trim() != "```" {
                    out.push(mk(li, starts[li], true)); // verbatim body
                    li += 1;
                }
                if li < lines.len() {
                    out.push(mk(li, starts[li], true)); // closing fence
                    li += 1;
                }
                bi += 1;
            }
            Some(block) => {
                let prefix_len = lines[li].len().saturating_sub(block.text.len());
                out.push(mk(li, starts[li] + prefix_len, false));
                li += 1;
                bi += 1;
            }
            None => {
                out.push(mk(li, starts[li], false)); // no block (degenerate) — no prefix
                li += 1;
            }
        }
    }
    out
}

/// Compute the compose-field marker treatment for a caret at byte offset `caret`. Inline
/// emphasis markers hide (the run under the caret reveals → dim instead); structural prefixes
/// dim off the caret's line (shown plain on it, so the raw marker is editable). Content styling
/// is unchanged — each app styles the non-marker [`decoration_map`] kinds as before; this
/// only decides marker visibility. The inline/structural split reuses [`crate::notes::parse_note`]
/// and the reveal set is [`inline_reveal_ranges`], so web and native never drift.
pub fn compose_decoration_plan(src: &str, caret: usize) -> ComposeMarkerPlan {
    let mut plan = ComposeMarkerPlan::default();
    let decos = decoration_map(src);
    if decos.is_empty() {
        return plan;
    }
    let lines = note_line_prefixes(src);
    let reveals = inline_reveal_ranges(src, caret);
    let caret_line = lines
        .iter()
        .position(|l| caret >= l.start && caret <= l.end);
    let is_revealed =
        |start: usize, end: usize| reveals.iter().any(|r| r.start <= start && end <= r.end);

    for d in &decos {
        if !matches!(
            d.kind,
            MdDecorationKind::Marker | MdDecorationKind::ListMarker
        ) {
            continue; // content kind — each app styles it from decoration_map, unchanged
        }
        let Some((li, line)) = lines
            .iter()
            .enumerate()
            .find(|(_, l)| d.start >= l.start && d.start <= l.end)
        else {
            continue;
        };
        if line.is_code {
            continue; // fenced ``` marker — literal
        }
        let span = RevealSpan {
            start: d.start,
            end: d.end,
        };
        if d.start >= line.start && d.end <= line.prefix_end {
            if Some(li) != caret_line {
                plan.dim.push(span); // structural prefix off the caret line → dim
            }
            // on the caret line → shown plain (omitted)
        } else if is_revealed(d.start, d.end) {
            plan.dim.push(span); // inline emphasis, caret in its run → reveal (dim, editable)
        } else {
            plan.hide.push(span); // inline emphasis, caret away → conceal
        }
    }
    plan
}

/// 0-based line index (counting `\n`) of a byte offset into `src` — the number of newlines
/// strictly before `off` (clamped to `src.len()`). The caret-line rule for
/// [`compose_show_markers_dim_ranges`]; matches each app's own computation (windows/android
/// `lineOf`, GTK `TextIter::line`).
fn line_index(src: &str, off: usize) -> usize {
    let clamped = off.min(src.len());
    src.as_bytes()[..clamped]
        .iter()
        .filter(|&&b| b == b'\n')
        .count()
}

/// The compose-field **"show markers" (dimmed live-preview) mode** reveal policy — the whole-line
/// counterpart of [`compose_decoration_plan`]'s hide-by-default per-run reveal.
///
/// In this mode every syntax marker is drawn, but a marker shows **DIMMED** unless it sits on the
/// caret's current line, where it shows at full opacity so the raw markdown is editable (Obsidian's
/// "source on the active line"). This returns the marker byte-ranges to DIM: every
/// [`MdDecorationKind::Marker`] / [`MdDecorationKind::ListMarker`] decoration (inline emphasis
/// `*`/`**`/`` ` ``/`[]()`, the ` ``` ` fence, and the structural `#`/`>`/`-`/`1.` prefixes) whose
/// source line differs from the caret's line. Markers on the caret's line are revealed (not
/// returned); content styling is unchanged — each app styles the non-marker [`decoration_map`]
/// kinds as before.
///
/// One definition so windows (`ComposeMarkdownDecorator`), android (`MarkdownCompose`), and linux
/// (`compose_decoration.rs`) share the caret-line reveal rule instead of each re-deriving "is this
/// marker on the caret's line" (priority #2/#4). Unlike [`compose_decoration_plan`] (which leaves
/// markers inside a fenced code block untouched), show-markers mode dims the ` ``` ` fence off the
/// caret line, matching the shipped clients. `caret` and the returned offsets are UTF-8 byte
/// indices into `src` (same space as [`decoration_map`]); the caller maps them to its native unit.
/// `src` is not `\r\n`-normalized, matching [`decoration_map`].
pub fn compose_show_markers_dim_ranges(src: &str, caret: usize) -> Vec<RevealSpan> {
    let decos = decoration_map(src);
    if decos.is_empty() {
        return Vec::new();
    }
    let caret_line = line_index(src, caret);
    decos
        .iter()
        .filter(|d| {
            matches!(
                d.kind,
                MdDecorationKind::Marker | MdDecorationKind::ListMarker
            )
        })
        .filter(|d| line_index(src, d.start) != caret_line)
        .map(|d| RevealSpan {
            start: d.start,
            end: d.end,
        })
        .collect()
}

struct ParsedLink {
    span: MdSpan,
    consumed: usize,
    /// Char index of the `]` closing the label — lets the inline scanner split the
    /// `[label` opener from the `](url)` tail into separate marker ranges.
    close_bracket: usize,
}

/// Try to parse `[label](url)` at `start` (where `chars[start] == '['`), requiring an
/// `http(s)://` url with no `)` inside it (mirrors web's link regex).
fn parse_link(chars: &[char], start: usize) -> Option<ParsedLink> {
    let close_bracket = find_char(chars, start + 1, ']')?;
    if close_bracket == start + 1 {
        return None; // empty label ([^\]]+ requires ≥1 char)
    }
    let open_paren = close_bracket + 1;
    if open_paren >= chars.len() || chars[open_paren] != '(' {
        return None;
    }
    let close_paren = find_char(chars, open_paren + 1, ')')?;
    let url: String = chars[open_paren + 1..close_paren].iter().collect();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return None;
    }
    let label: String = chars[start + 1..close_bracket].iter().collect();
    Some(ParsedLink {
        span: MdSpan {
            text: label,
            link: url,
            ..Default::default()
        },
        consumed: close_paren + 1 - start,
        close_bracket,
    })
}

/// Index of the next `needle` at or after `from`, or `None`.
fn find_char(chars: &[char], from: usize, needle: char) -> Option<usize> {
    (from..chars.len()).find(|&j| chars[j] == needle)
}

/// Index of the start of the next occurrence of `seq` at or after `from`, or `None`.
fn find_seq(chars: &[char], from: usize, seq: &[char]) -> Option<usize> {
    if seq.is_empty() || from >= chars.len() {
        return None;
    }
    (from..=chars.len().saturating_sub(seq.len())).find(|&j| chars[j..j + seq.len()] == *seq)
}

/// CommonMark forbids *intraword* underscore emphasis: an opening `_` run that sits
/// directly after an alphanumeric (`snake_case`, `a_b_c`) is literal text, not a
/// delimiter. `at` is the index of the run's first `_`. (Asterisk emphasis has no
/// such restriction — `a*b*c` italicises `b`.)
fn underscore_opens(chars: &[char], at: usize) -> bool {
    at == 0 || !chars[at - 1].is_alphanumeric()
}

/// The mirror of [`underscore_opens`] for the closing run: a `_` directly before an
/// alphanumeric closes nothing. `after` is the index just past the closing `_` run.
fn underscore_closes(chars: &[char], after: usize) -> bool {
    after >= chars.len() || !chars[after].is_alphanumeric()
}

/// HTML-escape, matching web's `escapeHtml` (`&` first, then `<`, `>`, `"`).
fn escape_html(text: &str) -> String {
    escape_html_with(text, false)
}

/// HTML-escape with an optional apostrophe rule (`'` → `&#39;`) on top of the
/// base four. Federated protocols (e.g. ActivityPub) commonly escape `'` too —
/// their peers' own encoders emit `&#39;` and their decoders must round-trip
/// it — so that call shape gets the extra rule via `true`; markdown rendering
/// (a `<p>`/`<code>` text-node context, never a quoted attribute) doesn't need
/// it and stays on the base four via [`escape_html`].
pub fn escape_html_with(text: &str, escape_apostrophe: bool) -> String {
    let escaped = text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;");
    if escape_apostrophe {
        escaped.replace('\'', "&#39;")
    } else {
        escaped
    }
}

fn render_spans(spans: &[MdSpan], img: RemoteImageMode) -> String {
    let mut out = String::new();
    for s in spans {
        let escaped = escape_html(&s.text);
        if s.is_image() {
            out.push_str(&render_image(&s.image_url, &s.text, img));
        } else if s.code {
            out.push_str(&format!("<code>{}</code>", escaped));
        } else if !s.link.is_empty() {
            out.push_str(&format!(
                "<a href=\"{}\" rel=\"noopener noreferrer\" target=\"_blank\">{}</a>",
                escape_html(&s.link),
                escaped
            ));
        } else if s.bold && s.italic {
            out.push_str(&format!("<strong><em>{}</em></strong>", escaped));
        } else if s.bold {
            out.push_str(&format!("<strong>{}</strong>", escaped));
        } else if s.italic {
            out.push_str(&format!("<em>{}</em>", escaped));
        } else {
            out.push_str(&escaped);
        }
    }
    out
}

/// How a renderer treats a `![alt](url)` **remote** image. Mail and DM bodies are
/// untrusted inbound content, so the privacy posture — never auto-fetch,
/// remote content blocked by default — is to render a placeholder
/// that carries the url **without fetching it**, revealed per-message by the
/// `load-remote-content-button`. Content the user authored, follows, or is sending
/// (outbound email, feed articles, nest-hosted web pages) uses [`Fetch`].
///
/// [`Fetch`]: RemoteImageMode::Fetch
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteImageMode {
    /// Emit `<img src="url">` — the renderer (or recipient MUA) loads the image.
    Fetch,
    /// Emit a no-`src` placeholder `<img alt data-remote-src="url"
    /// class="blocked-remote-image">` — **no network fetch**; a client reveal action
    /// swaps `data-remote-src` → `src`.
    Blocked,
}

/// Render one remote `![alt](url)` image to HTML per [`RemoteImageMode`].
fn render_image(url: &str, alt: &str, mode: RemoteImageMode) -> String {
    let alt = escape_html(alt);
    let url = escape_html(url);
    match mode {
        RemoteImageMode::Fetch => format!("<img src=\"{url}\" alt=\"{alt}\">"),
        // No `src` attribute → the browser performs no request until a reveal
        // action copies `data-remote-src` into `src`.
        RemoteImageMode::Blocked => {
            format!("<img alt=\"{alt}\" data-remote-src=\"{url}\" class=\"blocked-remote-image\">")
        }
    }
}

/// Render the token model to the canonical HTML string (the web app's output),
/// loading remote images. Equivalent to `render_html_with(blocks,
/// RemoteImageMode::Fetch)`; retained for existing callers.
pub fn render_html(blocks: &[MdBlock]) -> String {
    render_html_with(blocks, RemoteImageMode::Fetch)
}

/// Render the token model to the canonical HTML string, treating remote images per
/// `img` ([`RemoteImageMode::Blocked`] for untrusted inbound mail/DM bodies).
pub fn render_html_with(blocks: &[MdBlock], img: RemoteImageMode) -> String {
    let mut parts: Vec<String> = Vec::new();
    for b in blocks {
        match b.kind.as_str() {
            KIND_HEADING => {
                let inner = b
                    .lines
                    .first()
                    .map(|l| render_spans(&l.spans, img))
                    .unwrap_or_default();
                parts.push(format!("<h{0}>{1}</h{0}>", b.level, inner));
            }
            KIND_PARAGRAPH => {
                let inner = b
                    .lines
                    .first()
                    .map(|l| render_spans(&l.spans, img))
                    .unwrap_or_default();
                parts.push(format!("<p>{}</p>", inner));
            }
            KIND_LIST => {
                let items: String = b
                    .lines
                    .iter()
                    .map(|l| format!("<li>{}</li>", render_spans(&l.spans, img)))
                    .collect();
                parts.push(format!("<ul>{}</ul>", items));
            }
            KIND_ORDERED_LIST => {
                let items: String = b
                    .lines
                    .iter()
                    .map(|l| format!("<li>{}</li>", render_spans(&l.spans, img)))
                    .collect();
                // `<ol>` renumbers from 1 — the model carries no item numbers.
                parts.push(format!("<ol>{}</ol>", items));
            }
            KIND_BLOCKQUOTE => {
                let inner = b
                    .lines
                    .first()
                    .map(|l| render_spans(&l.spans, img))
                    .unwrap_or_default();
                parts.push(format!("<blockquote>{}</blockquote>", inner));
            }
            KIND_CODE_BLOCK => {
                parts.push(format!("<pre><code>{}</code></pre>", escape_html(&b.code)));
            }
            _ => {}
        }
    }
    parts.join("\n")
}

/// Parse + render in one step — the web app's `markdownToHtml(md)`. Remote images
/// load ([`RemoteImageMode::Fetch`]); use [`markdown_to_html_blocked`] for untrusted
/// inbound mail/DM bodies.
pub fn markdown_to_html(md: &str) -> String {
    render_html(&parse_markdown(md))
}

/// Parse + render with remote images **blocked** ([`RemoteImageMode::Blocked`]) — the
/// safe render path for an inbound mail / DM body (`BodyFormat::Markdown`). Remote
/// `![]()` images become no-`src` placeholders the `load-remote-content-button`
/// reveals per-message; nothing is fetched until the user opts in.
pub fn markdown_to_html_blocked(md: &str) -> String {
    render_html_with(&parse_markdown(md), RemoteImageMode::Blocked)
}

/// Count the remote `![alt](url)` images in a Markdown body — lets a client decide
/// whether to show the per-message `load-remote-content-button` (shown only when a
/// body has ≥1 blocked remote image).
pub fn count_remote_images(md: &str) -> usize {
    parse_markdown(md)
        .iter()
        .flat_map(|b| b.lines.iter())
        .flat_map(|l| l.spans.iter())
        .filter(|s| s.is_image())
        .count()
}

/// Flatten Markdown to a single-line plaintext preview — the canonical shared
/// way to strip Markdown markers for snippets (e.g. the conversation-list row:
/// `**bold**` → `bold`). Walks the same [`parse_markdown`] token model every
/// app renders, so the stripped text matches the formatted view minus
/// styling; it never strips more than the renderer recognizes (an unterminated
/// `**` stays literal). Block-level structure (headings, paragraphs, list items,
/// code blocks) and runs of whitespace fold into one space-separated line — a
/// preview is one line, not a rendered document. Shared (WASM + native) so all
/// six apps derive previews identically (priority #1/#2).
pub fn markdown_to_plaintext(md: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    for b in parse_markdown(md) {
        match b.kind.as_str() {
            KIND_CODE_BLOCK => {
                if !b.code.is_empty() {
                    parts.push(b.code);
                }
            }
            // heading / paragraph / list: concatenate each line's span texts
            // (link spans contribute their label, not the href).
            _ => {
                for line in &b.lines {
                    let text: String = line.spans.iter().map(|s| s.text.as_str()).collect();
                    if !text.is_empty() {
                        parts.push(text);
                    }
                }
            }
        }
    }
    // Collapse all whitespace (block joins, multi-line code, parse-inserted
    // spaces) into single spaces for a clean one-line preview.
    parts
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Result of [`wrap_selection`] — how a markdown authoring toolbar should replace the
/// current selection when toggling an inline style (bold/italic/code).
///
/// The caller splices it back in its own native string indexing (UTF-16 for
/// C#/JS/Kotlin/Swift, bytes for Rust/GTK). The offsets stay correct across encodings
/// because they are derived from the *returned substrings'* native lengths, never from
/// indices computed in Rust:
/// ```text
/// new_text      = source[..sel_start] + replacement + source[sel_end..]
/// new_sel_start = sel_start + len(before_core)   // re-select the wrapped core
/// new_sel_len   = len(core)
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SelectionWrap {
    /// Replaces the selected region `[sel_start, sel_end)` in the source text.
    pub replacement: String,
    /// The part of `replacement` before the re-selectable core: any leading whitespace
    /// kept *outside* the opening marker, followed by `prefix`.
    pub before_core: String,
    /// The non-whitespace text now wrapped in markers — what the caller re-selects.
    pub core: String,
}

/// Wrap `selected` in markdown `prefix`/`suffix` markers for an authoring toolbar,
/// keeping any leading/trailing whitespace OUTSIDE the markers.
///
/// The per-app toolbars used to wrap the raw selection (`prefix + selected +
/// suffix`); when a double-click word-selection included the trailing space, that
/// produced `*italic *`, which (a) collides with an adjacent span into
/// `*italic ***bold**` and (b) isn't even valid CommonMark emphasis (a marker next to
/// inner whitespace can't open/close). Trimming the whitespace to the outside fixes
/// both: `*italic* ` then `**bold**` → `*italic* **bold**`. The one shared home for
/// this (priority #1/#2/#4), alongside [`parse_markdown`] — clients call it over
/// UniFFI/WASM instead of each re-deriving it.
///
/// Empty or all-whitespace selections wrap `placeholder` instead (pass `""` for a bare
/// `prefix``suffix` with the cursor between the markers).
pub fn wrap_selection(
    selected: &str,
    prefix: &str,
    suffix: &str,
    placeholder: &str,
) -> SelectionWrap {
    let core = selected.trim();
    if core.is_empty() {
        // Empty or all-whitespace selection: keep any selected whitespace outside the
        // markers and wrap the placeholder. An empty placeholder leaves an empty core,
        // so the caller drops the cursor between the markers.
        return SelectionWrap {
            replacement: format!("{selected}{prefix}{placeholder}{suffix}"),
            before_core: format!("{selected}{prefix}"),
            core: placeholder.to_string(),
        };
    }
    // `trim_start`/`trim_end` return valid &str slices, so these byte offsets always
    // land on char boundaries — no mid-codepoint slicing.
    let lead = &selected[..selected.len() - selected.trim_start().len()];
    let trail = &selected[selected.trim_end().len()..];
    SelectionWrap {
        replacement: format!("{lead}{prefix}{core}{suffix}{trail}"),
        before_core: format!("{lead}{prefix}"),
        core: core.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── escape_html_with: base four vs. the ActivityPub-only apostrophe rule ──

    #[test]
    fn escape_html_with_false_matches_the_base_four() {
        assert_eq!(
            escape_html_with("<a> & \"b\" 'c'", false),
            "&lt;a&gt; &amp; &quot;b&quot; 'c'"
        );
        // escape_html() itself stays on the base four (its markdown callers
        // render into text nodes, never a quoted attribute).
        assert_eq!(
            escape_html("<a> & \"b\" 'c'"),
            "&lt;a&gt; &amp; &quot;b&quot; 'c'"
        );
    }

    #[test]
    fn escape_html_with_true_also_escapes_apostrophes() {
        assert_eq!(
            escape_html_with("<a> & \"b\" 'c'", true),
            "&lt;a&gt; &amp; &quot;b&quot; &#39;c&#39;"
        );
    }

    // ── Authoring toolbar: wrap_selection keeps edge whitespace outside markers ──

    #[test]
    fn wrap_selection_plain_word() {
        let w = wrap_selection("word", "**", "**", "text");
        assert_eq!(w.replacement, "**word**");
        assert_eq!(w.before_core, "**");
        assert_eq!(w.core, "word");
    }

    #[test]
    fn wrap_selection_trailing_space_stays_outside() {
        // The reported bug: double-click "italic" selected "italic " (trailing space);
        // wrapping the raw selection produced `*italic *`, which then collided with the
        // adjacent bold into `*italic ***bold**`. The space must stay outside the markers.
        let w = wrap_selection("italic ", "*", "*", "text");
        assert_eq!(w.replacement, "*italic* ");
        assert_eq!(w.before_core, "*");
        assert_eq!(w.core, "italic");
    }

    #[test]
    fn wrap_selection_leading_space_stays_outside() {
        let w = wrap_selection(" bold", "**", "**", "text");
        assert_eq!(w.replacement, " **bold**");
        assert_eq!(w.before_core, " **");
        assert_eq!(w.core, "bold");
    }

    #[test]
    fn wrap_selection_both_sides_whitespace() {
        let w = wrap_selection("  word\t", "*", "*", "text");
        assert_eq!(w.replacement, "  *word*\t");
        assert_eq!(w.before_core, "  *");
        assert_eq!(w.core, "word");
    }

    #[test]
    fn wrap_selection_empty_uses_placeholder() {
        let w = wrap_selection("", "*", "*", "text");
        assert_eq!(w.replacement, "*text*");
        assert_eq!(w.before_core, "*");
        assert_eq!(w.core, "text");
    }

    #[test]
    fn wrap_selection_all_whitespace_uses_placeholder_keeping_space() {
        let w = wrap_selection("  ", "*", "*", "text");
        assert_eq!(w.replacement, "  *text*");
        assert_eq!(w.before_core, "  *");
        assert_eq!(w.core, "text");
    }

    #[test]
    fn wrap_selection_empty_blank_placeholder_is_cursor_between_markers() {
        // Passing an empty placeholder yields a bare `**`/`**` with an empty core, so
        // the caller places the cursor between the markers (linux's old behavior).
        let w = wrap_selection("", "**", "**", "");
        assert_eq!(w.replacement, "****");
        assert_eq!(w.before_core, "**");
        assert_eq!(w.core, "");
    }

    #[test]
    fn wrap_selection_unicode_core_is_char_safe() {
        // Proves the trim/slice is on char boundaries (naive Rust byte-slicing would
        // panic mid-codepoint) and that caller-side length math works for multi-byte cores.
        let w = wrap_selection("héllo ", "*", "*", "text");
        assert_eq!(w.replacement, "*héllo* ");
        assert_eq!(w.before_core, "*");
        assert_eq!(w.core, "héllo");
    }

    // ── Notes editor: inline_reveal_ranges (caret-edge per-run reveal) ──
    //
    // Hand-computed byte offsets over the source string are the contract (independent of the
    // implementation). A run reveals when the caret is in [open.start, close.end] inclusive.

    fn reveal(src: &str, caret: usize) -> Vec<(usize, usize)> {
        inline_reveal_ranges(src, caret)
            .into_iter()
            .map(|r| (r.start, r.end))
            .collect()
    }

    #[test]
    fn reveal_caret_in_plain_text_reveals_nothing() {
        // "Buy **milk** and `code` now." — caret in "Buy" (offset 0) and on the space
        // before the run (offset 3) reveal nothing.
        let s = "Buy **milk** and `code` now.";
        assert_eq!(reveal(s, 0), vec![]);
        assert_eq!(reveal(s, 3), vec![]);
    }

    #[test]
    fn reveal_caret_inside_bold_run_reveals_only_its_markers() {
        // "Buy **milk** and `code` now.": **milk** markers = [4,6) + [10,12); caret at 7
        // (inside "milk") reveals exactly those — NOT the later `code` backticks (locality).
        let s = "Buy **milk** and `code` now.";
        assert_eq!(reveal(s, 7), vec![(4, 6), (10, 12)]);
    }

    #[test]
    fn reveal_run_edges_are_inclusive() {
        // Caret exactly at the run start (4 = open `**` start) and run end (12 = close `**`
        // end) both reveal the run; one past either edge does not.
        let s = "Buy **milk** and `code` now.";
        assert_eq!(reveal(s, 4), vec![(4, 6), (10, 12)]);
        assert_eq!(reveal(s, 12), vec![(4, 6), (10, 12)]);
        assert_eq!(reveal(s, 13), vec![]); // in "and", past milk, before code
    }

    #[test]
    fn reveal_caret_inside_code_run_reveals_only_backticks() {
        // `code` backticks = [17,18) + [22,23); caret at 19 (inside "code") reveals those only.
        let s = "Buy **milk** and `code` now.";
        assert_eq!(reveal(s, 19), vec![(17, 18), (22, 23)]);
        assert_eq!(reveal(s, 23), vec![(17, 18), (22, 23)]); // run-end edge
        assert_eq!(reveal(s, 24), vec![]); // one past the closing backtick
    }

    #[test]
    fn reveal_skips_structural_heading_prefix() {
        // A heading `#` is block chrome, never an inline marker: "# Plan" reveals nothing
        // anywhere (content "Plan" has no inline runs).
        assert_eq!(reveal("# Plan", 0), vec![]);
        assert_eq!(reveal("# Plan", 3), vec![]);
    }

    #[test]
    fn reveal_inline_run_inside_a_list_item_but_not_the_list_marker() {
        // "- buy **milk**": the `- ` prefix is structural (never revealed); the inline
        // **milk** inside the item (markers [6,8) + [12,14)) still reveals on caret-edge.
        let s = "- buy **milk**";
        assert_eq!(reveal(s, 9), vec![(6, 8), (12, 14)]); // caret in "milk"
        assert_eq!(reveal(s, 0), vec![]); // caret on the `-` marker — never revealed
    }

    #[test]
    fn reveal_inline_run_inside_a_blockquote_but_not_the_quote_marker() {
        // "> a `b`": `> ` is structural; the inline `b` (backticks [4,5) + [6,7)) reveals.
        let s = "> a `b`";
        assert_eq!(reveal(s, 5), vec![(4, 5), (6, 7)]);
        assert_eq!(reveal(s, 0), vec![]); // on the `>` marker
    }

    #[test]
    fn reveal_does_not_recognize_checkbox_syntax_as_inline() {
        // "- [ ] **go**": the `[ ]` checkbox is structural state (handled by the block model
        // / chrome widget), not an inline link — only the **go** run (markers [6,8) + [10,12))
        // reveals; a caret in "[ ]" reveals nothing.
        let s = "- [ ] **go**";
        assert_eq!(reveal(s, 9), vec![(6, 8), (10, 12)]); // caret in "go"
        assert_eq!(reveal(s, 3), vec![]); // caret in "[ ]"
    }

    #[test]
    fn reveal_is_per_line_not_cross_line() {
        // "a **b**\n**c** d": a caret in line 1's run reveals only line 1's markers, and a
        // caret in line 2's run only line 2's — proving the line walk rebases correctly.
        let s = "a **b**\n**c** d";
        assert_eq!(reveal(s, 4), vec![(2, 4), (5, 7)]); // caret in line-1 "b"
        assert_eq!(reveal(s, 10), vec![(8, 10), (11, 13)]); // caret in line-2 "c"
    }

    #[test]
    fn reveal_ignores_crlf_carriage_return_in_offsets() {
        // CRLF: the trailing `\r` is not content but offsets are NOT shifted (matches
        // decoration_map). "a **b**\r\nc" — the `**b**` run is still [2,4) + [5,7).
        let s = "a **b**\r\nc";
        assert_eq!(reveal(s, 4), vec![(2, 4), (5, 7)]);
    }

    #[test]
    fn reveal_never_inside_fenced_code() {
        // Fenced code content is structural (a code block), never revealed: "```\n**x**\n```"
        // with the caret inside the fenced `**x**` (offset 6) reveals nothing.
        let s = "```\n**x**\n```";
        assert_eq!(reveal(s, 6), vec![]);
    }

    #[test]
    fn reveal_link_run_reveals_bracket_and_url_tail() {
        // "[x](https://a.io)": markers = `[` [0,1) and `](https://a.io)` [2,17); a caret in
        // the label reveals both (so the whole link source, incl. the url, becomes editable).
        let s = "[x](https://a.io)";
        assert_eq!(reveal(s, 1), vec![(0, 1), (2, 17)]);
    }

    // ── HTML parity with web's markdown.ts (non-nested inline = byte-identical) ──

    #[test]
    fn paragraph_with_bold_italic_code() {
        assert_eq!(
            markdown_to_html("Hello **world** and *you* and `x`"),
            "<p>Hello <strong>world</strong> and <em>you</em> and <code>x</code></p>"
        );
    }

    #[test]
    fn underscore_emphasis_forms() {
        // The underscore family mirrors the asterisk family: `_x_` → <em>,
        // `__x__` → <strong>, `___x___` → <strong><em>. Required because the
        // inbound HTML→markdown converter emits `_…_` for <em>/<i> (without this,
        // every received HTML email's italics rendered as literal underscores).
        assert_eq!(
            markdown_to_html("an _italic_ word"),
            "<p>an <em>italic</em> word</p>"
        );
        assert_eq!(
            markdown_to_html("a __bold__ word"),
            "<p>a <strong>bold</strong> word</p>"
        );
        assert_eq!(
            markdown_to_html("a ___both___ word"),
            "<p>a <strong><em>both</em></strong> word</p>"
        );
        // htmd's actual output shape for `<p>a <em>b</em> <strong>c</strong></p>`.
        assert_eq!(
            markdown_to_html("a _b_ **c**"),
            "<p>a <em>b</em> <strong>c</strong></p>"
        );
        // The token model carries the flags too (native apps render from these).
        let spans = parse_inline("_x_");
        assert_eq!(spans.len(), 1);
        assert!(spans[0].italic && !spans[0].bold);
        assert_eq!(spans[0].text, "x");
    }

    #[test]
    fn underscore_emphasis_is_not_intraword() {
        // CommonMark forbids intraword underscore emphasis — these stay literal so
        // identifiers and URLs are not mangled. (Asterisk emphasis IS intraword.)
        assert_eq!(
            markdown_to_html("snake_case_name"),
            "<p>snake_case_name</p>"
        );
        assert_eq!(markdown_to_html("a_b_c"), "<p>a_b_c</p>");
        assert_eq!(
            markdown_to_html("see foo_bar_baz here"),
            "<p>see foo_bar_baz here</p>"
        );
        // A leading/trailing word boundary IS emphasis, even adjacent to punctuation.
        assert_eq!(markdown_to_html("(_hi_)"), "<p>(<em>hi</em>)</p>");
    }

    #[test]
    fn headings_levels_1_to_4() {
        assert_eq!(markdown_to_html("# H1"), "<h1>H1</h1>");
        assert_eq!(markdown_to_html("#### H4"), "<h4>H4</h4>");
        // 5 hashes is not a heading → paragraph.
        assert_eq!(markdown_to_html("##### H5"), "<p>##### H5</p>");
    }

    #[test]
    fn unordered_list_dash_star_plus() {
        assert_eq!(
            markdown_to_html("- a\n* b\n+ c"),
            "<ul><li>a</li><li>b</li><li>c</li></ul>"
        );
    }

    #[test]
    fn fenced_code_block_escapes() {
        assert_eq!(
            markdown_to_html("```\nlet x = a < b && c;\n```"),
            "<pre><code>let x = a &lt; b &amp;&amp; c;</code></pre>"
        );
    }

    #[test]
    fn link_only_http_s() {
        assert_eq!(
            markdown_to_html("see [docs](https://example.com/a)"),
            "<p>see <a href=\"https://example.com/a\" rel=\"noopener noreferrer\" target=\"_blank\">docs</a></p>"
        );
        // Non-http(s) target is not a link — stays literal.
        assert_eq!(markdown_to_html("[x](ftp://h)"), "<p>[x](ftp://h)</p>");
    }

    #[test]
    fn multiline_paragraph_joined_with_space() {
        assert_eq!(
            markdown_to_html("line one\nline two"),
            "<p>line one line two</p>"
        );
    }

    #[test]
    fn blocks_joined_with_newline() {
        assert_eq!(
            markdown_to_html("# Title\n\nBody para\n\n- item"),
            "<h1>Title</h1>\n<p>Body para</p>\n<ul><li>item</li></ul>"
        );
    }

    #[test]
    fn plain_text_is_escaped() {
        assert_eq!(
            markdown_to_html("a < b & \"c\""),
            "<p>a &lt; b &amp; &quot;c&quot;</p>"
        );
    }

    #[test]
    fn empty_input_is_empty() {
        assert_eq!(markdown_to_html(""), "");
        assert_eq!(markdown_to_html("\n\n"), "");
    }

    // ── token model (what the native apps render) ──

    #[test]
    fn tokens_heading() {
        let blocks = parse_markdown("## Hi **bold**");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, "heading");
        assert_eq!(blocks[0].level, 2);
        assert_eq!(blocks[0].lines.len(), 1);
        assert_eq!(
            blocks[0].lines[0].spans,
            vec![
                MdSpan::plain("Hi ".into()),
                MdSpan {
                    text: "bold".into(),
                    bold: true,
                    ..Default::default()
                },
            ]
        );
    }

    #[test]
    fn tokens_list_items() {
        let blocks = parse_markdown("- one\n- two");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, "list");
        assert_eq!(blocks[0].lines.len(), 2);
        assert_eq!(blocks[0].lines[0].spans, vec![MdSpan::plain("one".into())]);
        assert_eq!(blocks[0].lines[1].spans, vec![MdSpan::plain("two".into())]);
    }

    #[test]
    fn tokens_code_block_raw() {
        let blocks = parse_markdown("```\na<b\n```");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, "code_block");
        assert_eq!(blocks[0].code, "a<b");
        assert!(blocks[0].lines.is_empty());
    }

    #[test]
    fn tokens_link_span() {
        let spans = parse_inline("go [here](https://x.io) now");
        assert_eq!(
            spans,
            vec![
                MdSpan::plain("go ".into()),
                MdSpan {
                    text: "here".into(),
                    link: "https://x.io".into(),
                    ..Default::default()
                },
                MdSpan::plain(" now".into()),
            ]
        );
    }

    #[test]
    fn tokens_image_span() {
        // `![alt](http(s)://…)` → a remote-image span (alt in `text`, src in
        // `image_url`); the `!` binds to the image, not a literal `!` + link.
        let spans = parse_inline("see ![a cat](https://img.test/c.png) here");
        assert_eq!(
            spans,
            vec![
                MdSpan::plain("see ".into()),
                MdSpan {
                    text: "a cat".into(),
                    image_url: "https://img.test/c.png".into(),
                    ..Default::default()
                },
                MdSpan::plain(" here".into()),
            ]
        );
        assert!(spans[1].is_image());
    }

    #[test]
    fn image_non_http_scheme_is_not_an_image() {
        // Only `http(s)` image sources survive (mirrors the link gate); `data:` /
        // `javascript:` are never an image span (they stay literal, un-fetchable).
        for md in [
            "![x](data:image/png;base64,AAAA)",
            "![y](javascript:alert(1))",
        ] {
            let spans = parse_inline(md);
            assert!(
                spans.iter().all(|s| !s.is_image()),
                "must not be an image: {md:?} → {spans:?}"
            );
        }
    }

    #[test]
    fn html_loads_remote_image_in_fetch_mode() {
        // markdown_to_html (Fetch) emits a real `<img src>` — outbound mail,
        // feed articles, nest web-content.
        assert_eq!(
            markdown_to_html("![cat](https://img.test/c.png)"),
            "<p><img src=\"https://img.test/c.png\" alt=\"cat\"></p>"
        );
    }

    #[test]
    fn html_blocks_remote_image_in_blocked_mode() {
        // markdown_to_html_blocked (the inbound mail/DM render path) emits a
        // no-`src` placeholder carrying the url in `data-remote-src` — no fetch.
        let html = markdown_to_html_blocked("![cat](https://tracker.test/p.gif)");
        assert_eq!(
            html,
            "<p><img alt=\"cat\" data-remote-src=\"https://tracker.test/p.gif\" \
             class=\"blocked-remote-image\"></p>"
        );
        // The privacy invariant: a blocked image never carries a fetchable `src`
        // attribute (only the inert `data-remote-src`).
        assert!(
            !html.contains("<img src="),
            "blocked image must not fetch: {html}"
        );
    }

    #[test]
    fn count_remote_images_gates_the_button() {
        assert_eq!(count_remote_images("no images here **bold**"), 0);
        assert_eq!(
            count_remote_images("![a](https://x.test/a.png) and ![b](https://x.test/b.png)"),
            2
        );
        // A non-http image ref is inert, so it does not count.
        assert_eq!(count_remote_images("![x](data:image/png;base64,AA)"), 0);
    }

    #[test]
    fn plaintext_image_keeps_alt_text() {
        // A snippet preview shows the image's alt text, never its url.
        assert_eq!(
            markdown_to_plaintext("look ![a cat](https://img.test/c.png) now"),
            "look a cat now"
        );
    }

    #[test]
    fn unterminated_markers_are_literal() {
        assert_eq!(parse_inline("**oops"), vec![MdSpan::plain("**oops".into())]);
        assert_eq!(parse_inline("a `b"), vec![MdSpan::plain("a `b".into())]);
        assert_eq!(markdown_to_html("**oops"), "<p>**oops</p>");
    }

    // ── blockquote / ordered list / bold-italic (the superset over the local
    //    Linux DM-bubble parser this module replaces) ──

    #[test]
    fn tokens_blockquote() {
        // `> ` → a blockquote block (one line of inline content). Each quoted line is
        // its own block (mirrors the heading/paragraph single-line shape).
        let blocks = parse_markdown("> quoted **bold**\n> second line");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].kind, "blockquote");
        assert_eq!(
            blocks[0].lines[0].spans,
            vec![
                MdSpan::plain("quoted ".into()),
                MdSpan {
                    text: "bold".into(),
                    bold: true,
                    ..Default::default()
                },
            ]
        );
        assert_eq!(blocks[1].kind, "blockquote");
        assert_eq!(
            blocks[1].lines[0].spans,
            vec![MdSpan::plain("second line".into())]
        );
        // `>` without a trailing space is not a blockquote — stays a paragraph.
        let p = parse_markdown(">nospace");
        assert_eq!(p[0].kind, "paragraph");
    }

    #[test]
    fn tokens_ordered_list() {
        // `N. ` items group into one ordered_list block; the literal numbers are
        // discarded (renumber-from-1 at render).
        let blocks = parse_markdown("1. first\n2. second\n10. tenth");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, "ordered_list");
        assert_eq!(blocks[0].lines.len(), 3);
        assert_eq!(
            blocks[0].lines[0].spans,
            vec![MdSpan::plain("first".into())]
        );
        assert_eq!(
            blocks[0].lines[2].spans,
            vec![MdSpan::plain("tenth".into())]
        );
        // A non-digit prefix before `. ` is not an ordered item.
        assert_eq!(parse_markdown("Section 2. Details")[0].kind, "paragraph");
    }

    #[test]
    fn ordered_and_unordered_lists_do_not_merge() {
        // Switching bullet style closes the open list and opens a new one.
        let blocks = parse_markdown("- a\n- b\n1. c\n2. d");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].kind, "list");
        assert_eq!(blocks[0].lines.len(), 2);
        assert_eq!(blocks[1].kind, "ordered_list");
        assert_eq!(blocks[1].lines.len(), 2);
    }

    #[test]
    fn tokens_bold_italic_span() {
        // `***text***` → one span with BOTH bold and italic set.
        let spans = parse_inline("a ***both*** b");
        assert_eq!(
            spans,
            vec![
                MdSpan::plain("a ".into()),
                MdSpan {
                    text: "both".into(),
                    bold: true,
                    italic: true,
                    ..Default::default()
                },
                MdSpan::plain(" b".into()),
            ]
        );
        // `**bold**` and `*italic*` still parse as the single-style spans.
        assert_eq!(
            parse_inline("**b**"),
            vec![MdSpan {
                text: "b".into(),
                bold: true,
                ..Default::default()
            }]
        );
        assert_eq!(
            parse_inline("*i*"),
            vec![MdSpan {
                text: "i".into(),
                italic: true,
                ..Default::default()
            }]
        );
    }

    #[test]
    fn html_blockquote_ordered_list_bold_italic() {
        assert_eq!(
            markdown_to_html("> quoted text"),
            "<blockquote>quoted text</blockquote>"
        );
        assert_eq!(
            markdown_to_html("1. one\n2. two"),
            "<ol><li>one</li><li>two</li></ol>"
        );
        assert_eq!(
            markdown_to_html("***both***"),
            "<p><strong><em>both</em></strong></p>"
        );
    }

    #[test]
    fn plaintext_blockquote_ordered_bold_italic_flatten() {
        // Blockquote / ordered-list / bold-italic all fold into the one-line preview.
        assert_eq!(
            markdown_to_plaintext("> quote\n\n1. one\n2. two"),
            "quote one two"
        );
        assert_eq!(markdown_to_plaintext("***both***"), "both");
    }

    #[test]
    fn count_remote_images_inside_blockquote_and_ordered_list() {
        // Images carried inside a blockquote / ordered-list item are counted too —
        // they parse inline like any other block content.
        assert_eq!(count_remote_images("> q ![a](https://x.test/a.png)"), 1);
        assert_eq!(count_remote_images("1. ![b](https://x.test/b.png) item"), 1);
    }

    // ── plaintext flattening (snippet previews, e.g. the conversation list) ──

    #[test]
    fn plaintext_strips_inline_markers() {
        assert_eq!(markdown_to_plaintext("**bold**"), "bold");
        assert_eq!(
            markdown_to_plaintext("Hello **world** and *you* and `x`"),
            "Hello world and you and x"
        );
    }

    #[test]
    fn plaintext_link_keeps_label_drops_url() {
        assert_eq!(
            markdown_to_plaintext("see [docs](https://example.com/a)"),
            "see docs"
        );
    }

    #[test]
    fn plaintext_flattens_blocks_and_collapses_whitespace() {
        // Headings, paragraphs, and list items all fold into a single line —
        // a snippet is a one-line preview, not a rendered document.
        assert_eq!(
            markdown_to_plaintext("# Title\n\nBody para\n\n- item one\n- item two"),
            "Title Body para item one item two"
        );
        // A multi-line paragraph already joins with a space in parse; collapse
        // keeps it a single space.
        assert_eq!(
            markdown_to_plaintext("line one\nline two"),
            "line one line two"
        );
    }

    #[test]
    fn plaintext_code_block_is_text_only() {
        assert_eq!(markdown_to_plaintext("```\nlet x = 1;\n```"), "let x = 1;");
    }

    #[test]
    fn plaintext_empty_is_empty() {
        assert_eq!(markdown_to_plaintext(""), "");
        assert_eq!(markdown_to_plaintext("\n\n"), "");
    }

    #[test]
    fn plaintext_unterminated_marker_stays_literal() {
        // Parse leaves an unterminated `**` as literal text, so plaintext does
        // too — it never *adds* stripping beyond what the renderer recognizes.
        assert_eq!(markdown_to_plaintext("**oops"), "**oops");
    }

    // ── compose inline-styling decoration map (source byte ranges over the raw src) ──

    #[test]
    fn decoration_italic_marks_and_content() {
        use MdDecorationKind::*;
        // The canonical example: `*world*` → conceal the two `*` markers, style "world".
        assert_eq!(
            decoration_map("hello *world*"),
            vec![
                MdDecoration::new(6, 7, Marker),
                MdDecoration::new(7, 12, Italic),
                MdDecoration::new(12, 13, Marker),
            ]
        );
    }

    #[test]
    fn decoration_bold_code_bold_italic() {
        use MdDecorationKind::*;
        assert_eq!(
            decoration_map("**b**"),
            vec![
                MdDecoration::new(0, 2, Marker),
                MdDecoration::new(2, 3, Bold),
                MdDecoration::new(3, 5, Marker),
            ]
        );
        assert_eq!(
            decoration_map("`c`"),
            vec![
                MdDecoration::new(0, 1, Marker),
                MdDecoration::new(1, 2, Code),
                MdDecoration::new(2, 3, Marker),
            ]
        );
        assert_eq!(
            decoration_map("***x***"),
            vec![
                MdDecoration::new(0, 3, Marker),
                MdDecoration::new(3, 4, BoldItalic),
                MdDecoration::new(4, 7, Marker),
            ]
        );
    }

    #[test]
    fn decoration_underscore_forms_and_intraword() {
        use MdDecorationKind::*;
        assert_eq!(
            decoration_map("_i_"),
            vec![
                MdDecoration::new(0, 1, Marker),
                MdDecoration::new(1, 2, Italic),
                MdDecoration::new(2, 3, Marker),
            ]
        );
        // Intraword underscore stays literal (CommonMark) → no decoration, same as render.
        assert_eq!(decoration_map("snake_case_x"), vec![]);
    }

    #[test]
    fn decoration_link_and_image() {
        use MdDecorationKind::*;
        // `[` opener + `](url)` tail are markers; the label is the Link content.
        assert_eq!(
            decoration_map("see [docs](https://x.io)"),
            vec![
                MdDecoration::new(4, 5, Marker),
                MdDecoration::new(5, 9, Link),
                MdDecoration::new(9, 24, Marker),
            ]
        );
        // `![` opener + `](url)` tail are markers; the alt is the Image content.
        assert_eq!(
            decoration_map("![a](https://i.test/c.png)"),
            vec![
                MdDecoration::new(0, 2, Marker),
                MdDecoration::new(2, 3, Image),
                MdDecoration::new(3, 26, Marker),
            ]
        );
    }

    #[test]
    fn decoration_block_prefixes() {
        use MdDecorationKind::*;
        // Heading: `# ` marker + a Heading(level) style over the content.
        assert_eq!(
            decoration_map("# Title"),
            vec![
                MdDecoration::new(0, 2, Marker),
                MdDecoration::heading(2, 7, 1)
            ]
        );
        // Heading content carries inline decorations too (rebased past the `## `).
        assert_eq!(
            decoration_map("## Hi **b**"),
            vec![
                MdDecoration::new(0, 3, Marker),
                MdDecoration::heading(3, 11, 2),
                MdDecoration::new(6, 8, Marker),
                MdDecoration::new(8, 9, Bold),
                MdDecoration::new(9, 11, Marker),
            ]
        );
        // Blockquote: `> ` marker + a Blockquote style over the content.
        assert_eq!(
            decoration_map("> quoted"),
            vec![
                MdDecoration::new(0, 2, Marker),
                MdDecoration::new(2, 8, Blockquote),
            ]
        );
        // List markers (content is normal text → no extra style).
        assert_eq!(
            decoration_map("- item"),
            vec![MdDecoration::new(0, 2, ListMarker)]
        );
        assert_eq!(
            decoration_map("1. first"),
            vec![MdDecoration::new(0, 3, ListMarker)]
        );
    }

    #[test]
    fn decoration_fenced_code_block() {
        use MdDecorationKind::*;
        // Both fence lines are markers; the line(s) between are Code.
        assert_eq!(
            decoration_map("```\ncode\n```"),
            vec![
                MdDecoration::new(0, 3, Marker),
                MdDecoration::new(4, 8, Code),
                MdDecoration::new(9, 12, Marker),
            ]
        );
    }

    #[test]
    fn decoration_offsets_are_raw_bytes_multibyte_and_crlf() {
        use MdDecorationKind::*;
        // Multi-byte: "café " is 6 bytes (é = 2), so the marker starts at byte 6.
        assert_eq!(
            decoration_map("café *x*"),
            vec![
                MdDecoration::new(6, 7, Marker),
                MdDecoration::new(7, 8, Italic),
                MdDecoration::new(8, 9, Marker),
            ]
        );
        // CRLF is NOT normalized away: line 2 starts at byte 3 (a=0, \r=1, \n=2).
        assert_eq!(
            decoration_map("a\r\n*x*"),
            vec![
                MdDecoration::new(3, 4, Marker),
                MdDecoration::new(4, 5, Italic),
                MdDecoration::new(5, 6, Marker),
            ]
        );
    }

    #[test]
    fn decoration_unterminated_marker_is_literal() {
        // An in-progress / unterminated marker is literal in BOTH render and compose —
        // the shared scanner guarantees they agree (nothing styled while typing `*wor`).
        assert_eq!(decoration_map("hello *world"), vec![]);
        assert_eq!(decoration_map("a `b"), vec![]);
        assert_eq!(decoration_map(""), vec![]);
        assert_eq!(decoration_map("just plain text"), vec![]);
    }

    #[test]
    fn decoration_kind_str_matches_serde() {
        // The FFI/wasm string contract is snake_case and matches the serde rename, so web
        // (`decorationMap`) and native (`FfiMdDecoration.kind`) see the same token.
        assert_eq!(MdDecorationKind::BoldItalic.as_str(), "bold_italic");
        assert_eq!(MdDecorationKind::ListMarker.as_str(), "list_marker");
        assert_eq!(
            serde_json::to_string(&MdDecorationKind::BoldItalic).unwrap(),
            "\"bold_italic\""
        );
        assert_eq!(
            serde_json::to_string(&MdDecorationKind::ListMarker).unwrap(),
            "\"list_marker\""
        );
    }

    // ── compose_decoration_plan: inline markers HIDE, structural prefixes DIM ──────────────

    type SpanPairs = (Vec<(usize, usize)>, Vec<(usize, usize)>);

    fn cplan(src: &str, caret: usize) -> SpanPairs {
        let p = compose_decoration_plan(src, caret);
        let to = |v: Vec<RevealSpan>| v.into_iter().map(|r| (r.start, r.end)).collect();
        (to(p.hide), to(p.dim))
    }

    #[test]
    fn compose_inline_markers_hide_when_caret_away() {
        // "Buy **milk** now" — ** at [4,6)+[10,12); caret at 0 → both hidden, nothing dimmed.
        let (hide, dim) = cplan("Buy **milk** now", 0);
        assert_eq!(hide, vec![(4, 6), (10, 12)]);
        assert_eq!(dim, vec![]);
    }

    #[test]
    fn compose_inline_markers_dim_when_caret_in_run() {
        // Caret at 7 (inside "milk") reveals the ** → they DIM (editable), nothing hidden.
        let (hide, dim) = cplan("Buy **milk** now", 7);
        assert_eq!(hide, vec![]);
        assert_eq!(dim, vec![(4, 6), (10, 12)]);
    }

    #[test]
    fn compose_inline_code_markers_hide() {
        let (hide, dim) = cplan("`code`", 99);
        assert_eq!(hide, vec![(0, 1), (5, 6)]);
        assert_eq!(dim, vec![]);
    }

    #[test]
    fn compose_heading_prefix_dims_off_caret_line() {
        // "# Plan\nbody": caret in "body" (line 1, offset 8) → the "# " prefix dims, never hides.
        let (hide, dim) = cplan("# Plan\nbody", 8);
        assert_eq!(hide, vec![]);
        assert_eq!(dim, vec![(0, 2)]);
    }

    #[test]
    fn compose_heading_prefix_shown_plain_on_caret_line() {
        // Caret in "# Plan" (line 0, offset 3) → the "# " is shown plain (neither hidden nor dim).
        let (hide, dim) = cplan("# Plan\nbody", 3);
        assert_eq!(hide, vec![]);
        assert_eq!(dim, vec![]);
    }

    #[test]
    fn compose_list_prefix_dims_while_inline_inside_item_hides() {
        // "- **hi**\nx": caret on line 1 → the "- " list prefix dims; the inline ** inside hides.
        let (hide, dim) = cplan("- **hi**\nx", 9);
        assert_eq!(hide, vec![(2, 4), (6, 8)]);
        assert_eq!(dim, vec![(0, 2)]);
    }

    #[test]
    fn compose_leading_double_star_is_inline_not_structural() {
        // A line that merely STARTS with ** is a paragraph (no structural prefix) → ** hides.
        let (hide, dim) = cplan("**bold** text", 99);
        assert_eq!(hide, vec![(0, 2), (6, 8)]);
        assert_eq!(dim, vec![]);
    }

    #[test]
    fn compose_markers_inside_fenced_code_are_untouched() {
        // Inline-looking markers inside a code fence are literal — neither hidden nor dimmed.
        let (hide, dim) = cplan("```\na *b* c\n```", 99);
        assert_eq!(hide, vec![]);
        assert_eq!(dim, vec![]);
    }

    // ── compose_show_markers_dim_ranges: markers DIM off the caret line, revealed on it ──────

    fn cshow(src: &str, caret: usize) -> Vec<(usize, usize)> {
        compose_show_markers_dim_ranges(src, caret)
            .into_iter()
            .map(|r| (r.start, r.end))
            .collect()
    }

    #[test]
    fn compose_show_markers_dims_inline_markers_off_caret_line() {
        // "a\n*b*": the two `*` markers ([2,3)+[4,5)) sit on line 1. Caret on line 0 (byte 0) →
        // both dim; the "b" content is styled by decoration_map, never in the dim set.
        assert_eq!(cshow("a\n*b*", 0), vec![(2, 3), (4, 5)]);
    }

    #[test]
    fn compose_show_markers_reveals_markers_on_caret_line() {
        // Caret inside "b" (line 1, byte 3) → both `*` markers are on the caret's line → none dim.
        assert_eq!(cshow("a\n*b*", 3), vec![]);
    }

    #[test]
    fn compose_show_markers_dims_structural_prefix_off_caret_line() {
        // "# Plan\nbody": caret in "body" (line 1, byte 8) → the "# " heading prefix dims. On its
        // own line (byte 3) it is revealed (not returned).
        assert_eq!(cshow("# Plan\nbody", 8), vec![(0, 2)]);
        assert_eq!(cshow("# Plan\nbody", 3), vec![]);
    }

    #[test]
    fn compose_show_markers_dims_code_fence_off_caret_line() {
        // Unlike compose_decoration_plan (which leaves fenced markers untouched), show-markers mode
        // dims the ``` fence off the caret line (matching windows/linux). "```\na\n```": caret on
        // the body (line 1, byte 4) → both fences ([0,3)+[6,9)) dim; the body is a Code content
        // decoration, never in the dim set.
        assert_eq!(cshow("```\na\n```", 4), vec![(0, 3), (6, 9)]);
    }

    #[test]
    fn compose_show_markers_empty_for_plain_text() {
        assert_eq!(cshow("just plain text", 0), vec![]);
        assert_eq!(cshow("", 0), vec![]);
    }
}
