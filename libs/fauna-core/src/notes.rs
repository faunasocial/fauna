//! The Notes editor's substrate-agnostic **block document model** — the shared editor layer
//! every app (web leads) binds its rich-text widget to, plus the lossless markdown
//! round-trip and the pure structural-gesture engine.
//!
//! Cold-start contract tracked internally (§ The block-model sketch,
//! § Structural-gesture API). Surface authority:
//! `docs/goal/ui/spaces-documents.md` § "The document editor's target fidelity — Notes".
//!
//! The linchpin (spec § The linchpin): **structural markup is typed block state, never text;
//! inline markup is markdown characters in the block's `text`, hidden by decoration.** So a
//! Note is a flat, ordered list of [`Block`]s; each block's structural prefix (`- `, `# `,
//! `> `, `1. `, `- [ ] `, indentation) is **derived** from its typed columns (`kind`/`depth`/
//! `checked`/`level`), and only its inline emphasis (`**bold**`, `` `code` ``) lives as
//! characters in `text` (the [`markdown`](crate::markdown) inline layer styles + conceals
//! those — see [`inline_reveal_ranges`](crate::markdown::inline_reveal_ranges)).
//!
//! **Markdown is the lossless serialization, not the at-rest substrate.** [`serialize_note`]
//! emits `indent(depth) + prefix(kind, checked, level) + text` per block; [`parse_note`]
//! recovers the block from the line's prefix + leading indent. The model round-trips exactly
//! (`blocks → markdown → blocks` identity — the spec's Proof 1; see the tests).
//!
//! **Substrate-agnostic above the lowering seam.** The model + gestures are identical under
//! Fork-1 (text surgery on one markdown buffer) and Fork-2 (typed-block ECS); only the
//! client's op-lowering at the very bottom differs. The editor therefore does not wait on the
//! Fork-2 spike.

use serde::{Deserialize, Serialize};

/// A stable, opaque block identifier within a `NoteDocument` editing session. Order comes
/// from the [`NoteDocument::blocks`] vector; this id only has to be unique within the document
/// so gestures can name a block, the one before it, or a freshly-inserted one. Under Fork-2 it
/// maps to the ECS row PK; under Fork-1 the client mints sequential ids on parse and a fresh
/// `> max` for an insert/split. Never a positional index (split/insert would shift it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BlockId(pub u64);

/// The structural kind of a [`Block`]. The structural marker is **derived** from this (plus
/// `depth`/`checked`/`level`) by [`serialize_note`], never stored in `text`. `Raw` is the
/// escape hatch for any construct the schema does not model (GFM tables, footnotes, embedded
/// HTML) — its `text` is verbatim markdown that round-trips untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    Paragraph,
    Bullet,
    Ordered,
    Todo,
    Heading,
    Quote,
    Code,
    Raw,
}

impl BlockKind {
    /// True for the list-family kinds (bullet / ordered / todo) — the kinds Enter/Tab/
    /// Backspace gestures treat as list items (split into siblings, indent, outdent-on-empty).
    pub fn is_list(self) -> bool {
        matches!(
            self,
            BlockKind::Bullet | BlockKind::Ordered | BlockKind::Todo
        )
    }
}

/// One block of a [`NoteDocument`]. `text` is the block's **inline-markdown** line — inline
/// markers (`**`, `*`, `` ` ``, `[](…)`) ARE present in it (the editor hides + styles them);
/// for `Code`/`Raw` it is the verbatim body. `depth` is the indentation / list-nesting level
/// (0 = top level). `checked` applies to `Todo` only; `level` (1–4) to `Heading` only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    pub id: BlockId,
    pub kind: BlockKind,
    pub depth: u32,
    pub checked: bool,
    pub level: u8,
    pub text: String,
}

impl Block {
    /// True when the block carries no content — the empty-list-item case Enter/Backspace
    /// special-case (an empty bullet/todo on Enter outdents or exits the list).
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

/// A Note as a flat, ordered list of [`Block`]s (spec § The block-model sketch). Nesting is
/// the integer `depth` on a flat list (the natural markdown-indentation representation), so
/// indent/outdent is a single-cell update and reorder a list move.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NoteDocument {
    pub blocks: Vec<Block>,
}

impl NoteDocument {
    /// Position of `id` in [`Self::blocks`], or `None` if absent.
    pub fn index_of(&self, id: BlockId) -> Option<usize> {
        self.blocks.iter().position(|b| b.id == id)
    }

    /// The block with `id`, or `None`.
    pub fn block(&self, id: BlockId) -> Option<&Block> {
        self.blocks.iter().find(|b| b.id == id)
    }

    /// The largest block id in the document, or `None` when empty — the basis a Fork-1 client
    /// uses to mint a fresh `> max` id for a split/insert gesture.
    pub fn max_id(&self) -> Option<BlockId> {
        self.blocks.iter().map(|b| b.id).max()
    }
}

// ── Markdown round-trip ────────────────────────────────────────────────────────────────

/// Two spaces per indentation level — the unit [`serialize_note`] emits and [`parse_note`]
/// recognizes, so `depth` round-trips exactly. (A leading tab on ingestion also counts as one
/// level, but is normalized to spaces on the next serialize.)
const INDENT_UNIT: &str = "  ";

/// Parse a markdown string into a [`NoteDocument`] (spec § Markdown ingestion). Each source
/// line maps to one [`Block`]: `kind`/`level`/`checked` from the structural prefix, `depth`
/// from the leading indent, `text` from the remainder (inline-markdown verbatim). A fenced
/// ` ``` ` block folds its inner lines into one [`BlockKind::Code`] block (verbatim body). Any
/// line that matches no structural prefix is a [`BlockKind::Paragraph`] holding the verbatim
/// text, so even un-modeled constructs round-trip losslessly. Block ids are assigned
/// sequentially from 0.
pub fn parse_note(md: &str) -> NoteDocument {
    let normalized = md.replace("\r\n", "\n");
    let lines: Vec<&str> = normalized.split('\n').collect();
    let mut blocks: Vec<Block> = Vec::new();
    let mut next_id = 0u64;
    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        let (depth, rest) = leading_depth(line);
        // Fenced code block: collect verbatim inner lines until the closing fence.
        if rest.starts_with("```") {
            let mut body: Vec<&str> = Vec::new();
            let mut j = i + 1;
            while j < lines.len() && lines[j].trim() != "```" {
                body.push(lines[j]);
                j += 1;
            }
            blocks.push(Block {
                id: BlockId(next_id),
                kind: BlockKind::Code,
                depth,
                checked: false,
                level: 0,
                text: body.join("\n"),
            });
            next_id += 1;
            // Skip the body and the closing fence (if present).
            i = if j < lines.len() { j + 1 } else { j };
            continue;
        }
        let (kind, checked, level, text) = classify_line(rest);
        blocks.push(Block {
            id: BlockId(next_id),
            kind,
            depth,
            checked,
            level,
            text,
        });
        next_id += 1;
        i += 1;
    }
    NoteDocument { blocks }
}

/// Serialize a [`NoteDocument`] back to markdown (spec § Markdown serialization). The inverse
/// of [`parse_note`]: `indent(depth) + prefix(kind, checked, level) + text` per block, joined
/// by `\n`. A `Code` block is emitted as a fenced ` ``` ` block (verbatim body, fences
/// indented to the block's depth). Ordered items emit `1. ` (the model carries no number —
/// markdown renumbers from 1, matching `<ol>`).
pub fn serialize_note(doc: &NoteDocument) -> String {
    let mut lines: Vec<String> = Vec::new();
    for b in &doc.blocks {
        let indent = INDENT_UNIT.repeat(b.depth as usize);
        match b.kind {
            BlockKind::Code => {
                lines.push(format!("{indent}```"));
                // The verbatim body may itself be multi-line.
                for body_line in b.text.split('\n') {
                    lines.push(body_line.to_string());
                }
                lines.push(format!("{indent}```"));
            }
            _ => {
                let prefix = match b.kind {
                    BlockKind::Paragraph | BlockKind::Raw => String::new(),
                    BlockKind::Bullet => "- ".to_string(),
                    BlockKind::Ordered => "1. ".to_string(),
                    BlockKind::Todo => {
                        if b.checked {
                            "- [x] ".to_string()
                        } else {
                            "- [ ] ".to_string()
                        }
                    }
                    BlockKind::Heading => {
                        let level = b.level.clamp(1, 4) as usize;
                        format!("{} ", "#".repeat(level))
                    }
                    BlockKind::Quote => "> ".to_string(),
                    BlockKind::Code => unreachable!("handled above"),
                };
                lines.push(format!("{indent}{prefix}{}", b.text));
            }
        }
    }
    lines.join("\n")
}

/// Count leading indentation in [`INDENT_UNIT`] (two-space) levels — a leading tab also counts
/// as one level — and return `(depth, remainder_after_indent)`.
fn leading_depth(line: &str) -> (u32, &str) {
    let mut depth = 0u32;
    let mut rest = line;
    loop {
        if let Some(r) = rest.strip_prefix('\t') {
            depth += 1;
            rest = r;
        } else if let Some(r) = rest.strip_prefix(INDENT_UNIT) {
            depth += 1;
            rest = r;
        } else {
            break;
        }
    }
    (depth, rest)
}

/// Classify a single (already de-indented) line into `(kind, checked, level, text)`. Task
/// syntax is checked before plain bullets; anything unmatched is a paragraph holding the
/// verbatim line. (Fenced code is handled by [`parse_note`], not here.)
fn classify_line(line: &str) -> (BlockKind, bool, u8, String) {
    // ATX heading: 1–4 '#' then whitespace then content.
    let hashes = line.bytes().take_while(|&b| b == b'#').count();
    if (1..=4).contains(&hashes) {
        let after = &line[hashes..];
        let content = after.trim_start_matches([' ', '\t']);
        if content.len() != after.len() && !content.is_empty() {
            return (BlockKind::Heading, false, hashes as u8, content.to_string());
        }
    }
    if let Some(c) = line.strip_prefix("> ") {
        return (BlockKind::Quote, false, 0, c.to_string());
    }
    // Task list (`- [ ] ` / `- [x] `, case-insensitive x) — before the plain bullet.
    if let Some(rest) = strip_list_marker(line) {
        if let Some(after) = rest.strip_prefix("[ ] ") {
            return (BlockKind::Todo, false, 0, after.to_string());
        }
        if let Some(after) = strip_checked_box(rest) {
            return (BlockKind::Todo, true, 0, after.to_string());
        }
        return (BlockKind::Bullet, false, 0, rest.to_string());
    }
    if let Some(rest) = strip_ordered_marker(line) {
        return (BlockKind::Ordered, false, 0, rest.to_string());
    }
    (BlockKind::Paragraph, false, 0, line.to_string())
}

/// `- `/`* `/`+ ` bullet marker → the remainder. Unlike the de-indent step, the marker must be
/// the first char (indentation was already stripped by [`leading_depth`]).
fn strip_list_marker(line: &str) -> Option<&str> {
    let rest = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .or_else(|| line.strip_prefix("+ "))?;
    Some(rest)
}

/// `[x] ` / `[X] ` checked-box prefix → the remainder.
fn strip_checked_box(rest: &str) -> Option<&str> {
    rest.strip_prefix("[x] ")
        .or_else(|| rest.strip_prefix("[X] "))
}

/// `N. ` ordered marker (one or more ASCII digits, then `. `) → the remainder. The number is
/// discarded (the model renumbers from 1 at serialize).
fn strip_ordered_marker(line: &str) -> Option<&str> {
    let dot = line.find(". ")?;
    let num = &line[..dot];
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(&line[dot + 2..])
}

// ── Per-line structural projection (the editor's render map) ─────────────────────────────

/// The role a buffer line plays in the rendered Notes editor. A fenced code block spans an
/// opening fence line, zero or more verbatim body lines, and a closing fence line — all ONE
/// [`BlockKind::Code`] block; every other block is exactly one [`NoteLineRole::Block`] line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteLineRole {
    Block,
    CodeOpen,
    CodeBody,
    CodeClose,
}

/// Per-buffer-line structural projection of a Notes markdown buffer, in **UTF-8 byte** offsets
/// (the substrate-agnostic offset unit — each app remaps to its native caret unit, e.g. web
/// → CodeMirror UTF-16). One entry per buffer line, in document order; produced by
/// [`note_line_map`]. It is the shared input every app's Notes view turns into chrome
/// (bullet / number / checkbox widget over the hidden structural prefix) + per-line styling, so
/// the structural derivation — code folding, checkbox indexing, ordered-list numbering, prefix
/// range — is computed once in shared Rust and never re-derived per client (priority #2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteLine {
    /// 0-based buffer line index.
    pub index: usize,
    /// Byte offset of the line's first char in the buffer.
    pub start: usize,
    /// Byte offset of the line's end (the position before its `\n`, or buffer end).
    pub end: usize,
    pub role: NoteLineRole,
    /// Index of the owning block in the `blocks` slice passed to [`note_line_map`] (every app
    /// already holds that slice — resolve the block by index). `usize::MAX` is the unreachable
    /// fallback for a `blocks` slice shorter than the buffer's logical lines.
    pub block: usize,
    /// Structural-prefix byte range to hide + replace with chrome (`prefix_from == prefix_to` ⇒
    /// none — paragraph / raw / code lines carry no editable-derived prefix). The prefix is the
    /// line's leading run before the block's `text` (indent + `- ` / `# ` / `> ` / `1. ` /
    /// `- [ ] `).
    pub prefix_from: usize,
    pub prefix_to: usize,
    /// 0-based index among the document's `todo` blocks (the indexed `document-checkbox-{n}` id),
    /// or `None` when the line is not a todo.
    pub checkbox_index: Option<usize>,
    /// 1-based number within the consecutive ordered-list run at this line's depth, or `None`
    /// when the line is not an ordered item. (Markdown renumbers from 1 — display-only.)
    pub ordered_number: Option<u32>,
}

/// Walk the buffer lines in lockstep with the [`parse_note`] blocks, projecting each to a
/// [`NoteLine`] (spec § The linchpin — structural markup is derived, never stored as text). The
/// mapping is exact because `parse_note` maps each non-code source line to one block and folds a
/// fenced ` ``` ` region into one [`BlockKind::Code`] block; we mirror that fold by consuming
/// buffer lines from the opening fence to the next line that trims to ` ``` ` (robust to
/// value↔serialize normalization — we read the real buffer, not the canonical serialization). A
/// non-code line's structural-prefix length is `line.len() - block.text.len()` (the prefix is
/// everything before the block's `text`, which is the line's byte-suffix by construction —
/// § Markdown ingestion), so the prefix is never re-classified here: `kind` / `depth` /
/// `checked` / `level` all come from `blocks`.
pub fn note_line_map(value: &str, blocks: &[Block]) -> Vec<NoteLine> {
    let lines: Vec<&str> = value.split('\n').collect();
    // Byte start of each buffer line.
    let mut starts: Vec<usize> = Vec::with_capacity(lines.len());
    let mut off = 0usize;
    for line in &lines {
        starts.push(off);
        off += line.len() + 1; // + '\n'
    }

    let mut out: Vec<NoteLine> = Vec::with_capacity(lines.len());
    // Running ordered-list number per depth; a non-ordered (or shallower) block resets the
    // deeper runs, a code block resets all.
    let mut ordered_run: std::collections::BTreeMap<u32, u32> = std::collections::BTreeMap::new();
    let mut checkbox = 0usize;
    let mut bi = 0usize;
    let mut li = 0usize;

    while li < lines.len() {
        if bi < blocks.len() && blocks[bi].kind == BlockKind::Code {
            // Opening fence at li; consume verbatim body until the closing fence (or buffer end).
            out.push(line_entry(
                li,
                starts[li],
                lines[li],
                blocks,
                bi,
                NoteLineRole::CodeOpen,
                None,
                None,
            ));
            li += 1;
            while li < lines.len() && lines[li].trim() != "```" {
                out.push(line_entry(
                    li,
                    starts[li],
                    lines[li],
                    blocks,
                    bi,
                    NoteLineRole::CodeBody,
                    None,
                    None,
                ));
                li += 1;
            }
            if li < lines.len() {
                out.push(line_entry(
                    li,
                    starts[li],
                    lines[li],
                    blocks,
                    bi,
                    NoteLineRole::CodeClose,
                    None,
                    None,
                ));
                li += 1;
            }
            ordered_run.clear();
            bi += 1;
            continue;
        }

        // A non-code block — exactly one buffer line. `kind`/`depth` come from the block (or the
        // unreachable fallback when `blocks` is short: an empty paragraph).
        let kind = blocks.get(bi).map(|b| b.kind);
        let depth = blocks.get(bi).map(|b| b.depth).unwrap_or(0);
        let mut checkbox_index = None;
        if kind == Some(BlockKind::Todo) {
            checkbox_index = Some(checkbox);
            checkbox += 1;
        }
        let mut ordered_number = None;
        if kind == Some(BlockKind::Ordered) {
            let n = ordered_run.get(&depth).copied().unwrap_or(0) + 1;
            ordered_run.insert(depth, n);
            ordered_number = Some(n);
        } else {
            // Any non-ordered block breaks ordered runs at its depth and deeper.
            ordered_run.retain(|&d, _| d < depth);
        }
        out.push(line_entry(
            li,
            starts[li],
            lines[li],
            blocks,
            bi,
            NoteLineRole::Block,
            checkbox_index,
            ordered_number,
        ));
        li += 1;
        bi += 1;
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn line_entry(
    index: usize,
    start: usize,
    line_text: &str,
    blocks: &[Block],
    bi: usize,
    role: NoteLineRole,
    checkbox_index: Option<usize>,
    ordered_number: Option<u32>,
) -> NoteLine {
    let end = start + line_text.len();
    let block = if bi < blocks.len() { bi } else { usize::MAX };
    // Structural prefix only for a `block` role: the leading run before the block's `text` (its
    // byte-suffix), so the prefix length is the byte-length difference.
    let prefix_to = if role == NoteLineRole::Block {
        let text_len = blocks.get(bi).map(|b| b.text.len()).unwrap_or(0);
        start + line_text.len().saturating_sub(text_len)
    } else {
        start
    };
    NoteLine {
        index,
        start,
        end,
        role,
        block,
        prefix_from: start,
        prefix_to,
        checkbox_index,
        ordered_number,
    }
}

// ── Structural gestures (pure engine, mirrors `markdown::wrap_selection`) ────────────────

/// A caret inside a block's `text`, as a UTF-8 byte offset (spec § Structural-gesture API).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockCaret {
    pub block: BlockId,
    pub offset: usize,
}

/// A structural editing gesture the editor routes through the shared engine instead of letting
/// the substrate edit text directly (spec § Structural-gesture API). The keymap maps one key to
/// one gesture; the engine decides the bullets-first behavior (split vs. outdent-on-empty,
/// indent clamp, outdent-before-merge) so all 7 apps share one policy (priority #1/#2).
///
/// `Newline` subsumes the spec's `Newline` + `NewlineOnEmpty`: emptiness is a property of the
/// document the engine reads, not a separate user gesture (Enter is one key) — so the client
/// never re-derives "is this list item empty?".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StructuralGesture {
    /// Enter: split a non-empty block at the caret; on an empty list item, outdent one level
    /// or (at depth 0) exit the list to a paragraph.
    Newline,
    /// Tab / the mobile indent button: nest one level (clamped to the previous block's depth + 1).
    Indent,
    /// Shift-Tab / the mobile outdent button: un-nest one level (clamped at 0).
    Outdent,
    /// Tap the checkbox / keyboard shortcut: flip a `Todo` block's `checked`.
    ToggleCheckbox,
    /// Backspace at offset 0: outdent first if nested, else merge into the previous block.
    BackspaceAtStart,
}

/// One atomic change to a [`NoteDocument`] (spec § Structural-gesture API). The client lowers
/// these to substrate ops: Fork-2 → `block_order` move / cell update / row insert+delete + text
/// CRDT ops; Fork-1 → [`apply_edits`] then re-serialize the one markdown buffer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockEdit {
    /// Truncate `at.block`'s text to `[..at.offset]`; the suffix `[at.offset..]` becomes a new
    /// block (`new_id`) right after it, of the [`split_child_kind`] of the original.
    SplitBlock {
        at: BlockCaret,
        new_id: BlockId,
    },
    /// Append `block`'s text to the previous block and delete `block`.
    MergeWithPrev {
        block: BlockId,
    },
    SetDepth {
        block: BlockId,
        depth: u32,
    },
    SetKind {
        block: BlockId,
        kind: BlockKind,
    },
    SetChecked {
        block: BlockId,
        checked: bool,
    },
    InsertBlock {
        after: Option<BlockId>,
        block: Block,
    },
}

/// The result of [`apply_structural_gesture`]: the atomic edits to lower, and the caret after
/// the gesture. An empty `edits` means the gesture was a no-op (the caret is unchanged and the
/// client falls back to default text handling — e.g. Enter inside a code block).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GestureResult {
    pub edits: Vec<BlockEdit>,
    pub caret: BlockCaret,
}

/// The kind a split-off suffix block takes from its originating block: a `Heading` does not
/// continue (its suffix is a `Paragraph`); every other kind continues as itself (a split `Todo`
/// yields a fresh **unchecked** `Todo`). Shared so every lowering (Fork-1 [`apply_edits`] and a
/// Fork-2 adapter) constructs the new block identically.
pub fn split_child_kind(kind: BlockKind) -> BlockKind {
    match kind {
        BlockKind::Heading => BlockKind::Paragraph,
        other => other,
    }
}

/// Apply a [`StructuralGesture`] at `caret`, returning the atomic [`BlockEdit`]s to lower and
/// the resulting caret. **Pure** (mirrors [`wrap_selection`](crate::markdown::wrap_selection)):
/// it never mutates `doc` and never mints ids — the caller supplies `new_id` (a fresh id, used
/// only by gestures that create a block; ignored otherwise). An out-of-document caret, or a
/// gesture with no structural effect, yields an empty edit list with the caret unchanged.
pub fn apply_structural_gesture(
    doc: &NoteDocument,
    caret: BlockCaret,
    gesture: StructuralGesture,
    new_id: BlockId,
) -> GestureResult {
    let noop = GestureResult {
        edits: Vec::new(),
        caret,
    };
    let Some(idx) = doc.index_of(caret.block) else {
        return noop;
    };
    let b = &doc.blocks[idx];
    match gesture {
        StructuralGesture::Newline => newline(doc, idx, caret, new_id),
        StructuralGesture::Indent => {
            // Clamp to the previous block's depth + 1 (can't nest past a non-existent parent);
            // the first block cannot indent at all.
            let max_depth = if idx == 0 {
                0
            } else {
                doc.blocks[idx - 1].depth + 1
            };
            let target = (b.depth + 1).min(max_depth);
            if target == b.depth {
                noop
            } else {
                GestureResult {
                    edits: vec![BlockEdit::SetDepth {
                        block: b.id,
                        depth: target,
                    }],
                    caret,
                }
            }
        }
        StructuralGesture::Outdent => {
            if b.depth == 0 {
                noop
            } else {
                GestureResult {
                    edits: vec![BlockEdit::SetDepth {
                        block: b.id,
                        depth: b.depth - 1,
                    }],
                    caret,
                }
            }
        }
        StructuralGesture::ToggleCheckbox => {
            if b.kind == BlockKind::Todo {
                GestureResult {
                    edits: vec![BlockEdit::SetChecked {
                        block: b.id,
                        checked: !b.checked,
                    }],
                    caret,
                }
            } else {
                noop
            }
        }
        StructuralGesture::BackspaceAtStart => {
            if caret.offset != 0 {
                return noop;
            }
            if b.depth > 0 {
                // Outdent before merge (Apple-Notes behavior).
                GestureResult {
                    edits: vec![BlockEdit::SetDepth {
                        block: b.id,
                        depth: b.depth - 1,
                    }],
                    caret,
                }
            } else if idx == 0 {
                noop // nothing to merge into
            } else {
                let prev = &doc.blocks[idx - 1];
                GestureResult {
                    edits: vec![BlockEdit::MergeWithPrev { block: b.id }],
                    caret: BlockCaret {
                        block: prev.id,
                        offset: prev.text.len(),
                    },
                }
            }
        }
    }
}

/// Enter handling — the spec's `Newline` + `NewlineOnEmpty` combined (see [`StructuralGesture`]).
fn newline(doc: &NoteDocument, idx: usize, caret: BlockCaret, new_id: BlockId) -> GestureResult {
    let b = &doc.blocks[idx];
    // Code/Raw blocks hold verbatim multi-line bodies: Enter inserts a literal newline INTO the
    // text, it does not split the block (splitting would emit a malformed double fence). Return
    // a no-op so the client falls back to default text insertion.
    if matches!(b.kind, BlockKind::Code | BlockKind::Raw) {
        return GestureResult {
            edits: Vec::new(),
            caret,
        };
    }
    // Enter on an empty list item: outdent one level, or (at depth 0) exit the list.
    if b.kind.is_list() && b.is_empty() {
        if b.depth > 0 {
            return GestureResult {
                edits: vec![BlockEdit::SetDepth {
                    block: b.id,
                    depth: b.depth - 1,
                }],
                caret: BlockCaret {
                    block: b.id,
                    offset: 0,
                },
            };
        }
        return GestureResult {
            edits: vec![BlockEdit::SetKind {
                block: b.id,
                kind: BlockKind::Paragraph,
            }],
            caret: BlockCaret {
                block: b.id,
                offset: 0,
            },
        };
    }
    // Otherwise split: the suffix after the caret moves to a new sibling block (its kind via
    // `split_child_kind`); the caret lands at the new block's start.
    GestureResult {
        edits: vec![BlockEdit::SplitBlock { at: caret, new_id }],
        caret: BlockCaret {
            block: new_id,
            offset: 0,
        },
    }
}

/// Apply a [`BlockEdit`] list to `doc`, producing the resulting [`NoteDocument`]. The Fork-1
/// reference lowering (text surgery on the in-memory block list) **and** the test oracle for
/// [`apply_structural_gesture`]; a Fork-2 client lowers each edit to atomic ECS ops instead but
/// must reach the same document. Edits are applied in order; an edit naming an absent block is a
/// no-op (defensive — a well-formed `GestureResult` never produces one).
pub fn apply_edits(doc: &NoteDocument, edits: &[BlockEdit]) -> NoteDocument {
    let mut out = doc.clone();
    for edit in edits {
        match edit {
            BlockEdit::SetDepth { block, depth } => {
                if let Some(b) = out.blocks.iter_mut().find(|b| b.id == *block) {
                    b.depth = *depth;
                }
            }
            BlockEdit::SetKind { block, kind } => {
                if let Some(b) = out.blocks.iter_mut().find(|b| b.id == *block) {
                    b.kind = *kind;
                    // Leaving the todo family clears its checkbox state.
                    if *kind != BlockKind::Todo {
                        b.checked = false;
                    }
                }
            }
            BlockEdit::SetChecked { block, checked } => {
                if let Some(b) = out.blocks.iter_mut().find(|b| b.id == *block) {
                    b.checked = *checked;
                }
            }
            BlockEdit::SplitBlock { at, new_id } => {
                if let Some(i) = out.index_of(at.block) {
                    let orig = &out.blocks[i];
                    let offset = at.offset.min(orig.text.len());
                    let suffix = orig.text[offset..].to_string();
                    let child = Block {
                        id: *new_id,
                        kind: split_child_kind(orig.kind),
                        depth: orig.depth,
                        checked: false,
                        level: 0,
                        text: suffix,
                    };
                    out.blocks[i].text.truncate(offset);
                    out.blocks.insert(i + 1, child);
                }
            }
            BlockEdit::MergeWithPrev { block } => {
                if let Some(i) = out.index_of(*block)
                    && i > 0
                {
                    let moved = out.blocks[i].text.clone();
                    out.blocks[i - 1].text.push_str(&moved);
                    out.blocks.remove(i);
                }
            }
            BlockEdit::InsertBlock { after, block } => {
                let at = match after {
                    Some(id) => out.index_of(*id).map(|i| i + 1).unwrap_or(out.blocks.len()),
                    None => 0,
                };
                out.blocks.insert(at, block.clone());
            }
        }
    }
    out
}

// ── Caret seam: whole-buffer byte caret ↔ BlockCaret ─────────────────────────────────────
//
// The gesture engine ([`apply_structural_gesture`]) speaks [`BlockCaret`] — a block id + a UTF-8
// byte offset INTO that block's `text`. A client's text widget speaks a WHOLE-BUFFER caret in its
// own unit (CodeMirror UTF-16, NSTextView UTF-16, a GTK byte offset…). These two functions are
// the substrate-agnostic core of that seam, in the same **UTF-8 byte** unit as [`note_line_map`]:
// the client converts its native whole-buffer caret to a byte offset (web: UTF-16→byte) and back,
// and this shared core does the block lookup / structural-prefix skip / code-body byte
// accumulation — the platform-agnostic arithmetic every native Notes editor would otherwise
// re-implement (priority #2). `value` is intentionally not a parameter: every line's byte length
// is `line.end - line.start`, so the arithmetic reads only the [`note_line_map`] output + the
// `blocks` slice (which the client already holds), never the buffer text.

/// The [`NoteLine`] containing whole-buffer **byte** offset `byte` — a line covers `[start, end]`
/// and the position at a line's `end` belongs to that line, not the next (a caret sitting at line
/// end). Falls back to the last line (an out-of-range caret clamps to the end).
fn line_at(line_map: &[NoteLine], byte: usize) -> Option<&NoteLine> {
    for l in line_map {
        if byte >= l.start && byte <= l.end {
            return Some(l);
        }
    }
    line_map.last()
}

/// Map a whole-buffer **byte** caret to a shared [`BlockCaret`] (block id + byte offset into the
/// block's `text`). `None` when the caret can't be resolved to a block (empty line map, or — the
/// unreachable case — a `blocks` slice shorter than the buffer's logical lines). For a [`Code`]
/// block the offset is into the joined verbatim body (`text` = body lines joined by `\n`); a caret
/// on the opening / closing fence maps to body start / body end respectively.
///
/// [`Code`]: BlockKind::Code
pub fn caret_to_block_caret(
    line_map: &[NoteLine],
    blocks: &[Block],
    caret_byte: usize,
) -> Option<BlockCaret> {
    let line = line_at(line_map, caret_byte)?;
    let block = blocks.get(line.block)?;
    match line.role {
        NoteLineRole::Block => {
            // Offset within `text` = caret minus the hidden structural prefix, clamped to the text
            // length. The prefix is the line's leading byte-run `[start, prefix_to)` and `text` is
            // the byte-suffix `[prefix_to, end)`, so the subtraction is exact.
            let within = caret_byte
                .saturating_sub(line.prefix_to)
                .min(block.text.len());
            Some(BlockCaret {
                block: block.id,
                offset: within,
            })
        }
        // A fence is not part of `text`: open → body start, close → body end.
        NoteLineRole::CodeOpen => Some(BlockCaret {
            block: block.id,
            offset: 0,
        }),
        NoteLineRole::CodeClose => Some(BlockCaret {
            block: block.id,
            offset: block.text.len(),
        }),
        NoteLineRole::CodeBody => {
            // Sum the byte lengths (+1 per joining `\n`) of this block's preceding body lines,
            // then add the within-line byte offset.
            let mut preceding = 0usize;
            for l in line_map {
                if l.block != line.block || l.role != NoteLineRole::CodeBody {
                    continue;
                }
                if l.index >= line.index {
                    break;
                }
                preceding += (l.end - l.start) + 1;
            }
            Some(BlockCaret {
                block: block.id,
                offset: preceding + caret_byte.saturating_sub(line.start),
            })
        }
    }
}

/// Map a shared [`BlockCaret`] back to a whole-buffer **byte** caret against `line_map` (built from
/// the post-gesture buffer + `blocks`). The inverse of [`caret_to_block_caret`] for a caret inside
/// a block's `text`; falls back to buffer start (`0`) when the block id can't be resolved.
pub fn block_caret_to_byte(line_map: &[NoteLine], blocks: &[Block], caret: BlockCaret) -> usize {
    // Non-code block: its single `block` line — hidden-prefix end + the within-`text` byte offset.
    if let Some(line) = line_map.iter().find(|l| {
        l.role == NoteLineRole::Block && blocks.get(l.block).map(|b| b.id) == Some(caret.block)
    }) {
        let text_len = blocks.get(line.block).map(|b| b.text.len()).unwrap_or(0);
        return line.prefix_to + caret.offset.min(text_len);
    }

    // Code block: walk body lines accumulating bytes until `caret.offset` lands.
    if let Some(open_line) = line_map.iter().find(|l| {
        l.role == NoteLineRole::CodeOpen && blocks.get(l.block).map(|b| b.id) == Some(caret.block)
    }) {
        let body: Vec<&NoteLine> = line_map
            .iter()
            .filter(|l| {
                l.role == NoteLineRole::CodeBody
                    && blocks.get(l.block).map(|b| b.id) == Some(caret.block)
            })
            .collect();
        if body.is_empty() {
            return open_line.end + 1; // empty body — the line after the fence
        }
        let mut remaining = caret.offset;
        for l in &body {
            let w = l.end - l.start;
            if remaining <= w {
                return l.start + remaining;
            }
            remaining -= w + 1; // + the joining '\n'
        }
        return body[body.len() - 1].end;
    }

    // Unknown block id — clamp to buffer start.
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blk(id: u64, kind: BlockKind, depth: u32, checked: bool, level: u8, text: &str) -> Block {
        Block {
            id: BlockId(id),
            kind,
            depth,
            checked,
            level,
            text: text.to_string(),
        }
    }

    // ── parse: line → block classification ──

    #[test]
    fn parse_classifies_each_block_kind() {
        let md = "# Plan\n## Sub\njust text\n- bullet\n1. first\n- [ ] todo\n- [x] done\n> quoted";
        let doc = parse_note(md);
        assert_eq!(
            doc.blocks,
            vec![
                blk(0, BlockKind::Heading, 0, false, 1, "Plan"),
                blk(1, BlockKind::Heading, 0, false, 2, "Sub"),
                blk(2, BlockKind::Paragraph, 0, false, 0, "just text"),
                blk(3, BlockKind::Bullet, 0, false, 0, "bullet"),
                blk(4, BlockKind::Ordered, 0, false, 0, "first"),
                blk(5, BlockKind::Todo, 0, false, 0, "todo"),
                blk(6, BlockKind::Todo, 0, true, 0, "done"),
                blk(7, BlockKind::Quote, 0, false, 0, "quoted"),
            ]
        );
    }

    #[test]
    fn parse_captures_nesting_depth_from_indent() {
        // Two spaces per level; a leading tab is also one level.
        let doc = parse_note("- a\n  - b\n    - c\n\t- d");
        let depths: Vec<u32> = doc.blocks.iter().map(|b| b.depth).collect();
        assert_eq!(depths, vec![0, 1, 2, 1]);
        assert_eq!(doc.blocks[1].text, "b");
        assert_eq!(doc.blocks[1].kind, BlockKind::Bullet);
    }

    #[test]
    fn parse_keeps_inline_markers_in_text() {
        // Inline emphasis stays as characters in `text` (the editor hides them; the model
        // never strips them) — this is what makes toggling bold a char edit, not a re-parse.
        let doc = parse_note("- buy **milk** and `code`");
        assert_eq!(doc.blocks[0].text, "buy **milk** and `code`");
    }

    #[test]
    fn parse_folds_fenced_code_into_one_block() {
        let doc = parse_note("before\n```\nlet x = 1;\nlet y = 2;\n```\nafter");
        assert_eq!(doc.blocks.len(), 3);
        assert_eq!(doc.blocks[1].kind, BlockKind::Code);
        assert_eq!(doc.blocks[1].text, "let x = 1;\nlet y = 2;");
        assert_eq!(doc.blocks[2].text, "after");
    }

    #[test]
    fn parse_empty_line_is_an_empty_paragraph() {
        let doc = parse_note("a\n\nb");
        assert_eq!(doc.blocks.len(), 3);
        assert_eq!(doc.blocks[1], blk(1, BlockKind::Paragraph, 0, false, 0, ""));
    }

    #[test]
    fn parse_unmodeled_construct_is_a_verbatim_paragraph() {
        // A GFM table line we don't model stays a paragraph holding the verbatim text, so it
        // round-trips losslessly (the spec's `raw` property without a dedicated kind yet).
        let doc = parse_note("| a | b |\n|---|---|");
        assert_eq!(doc.blocks[0].text, "| a | b |");
        assert_eq!(doc.blocks[1].text, "|---|---|");
        assert!(doc.blocks.iter().all(|b| b.kind == BlockKind::Paragraph));
    }

    // ── serialize: block → markdown prefix ──

    #[test]
    fn serialize_reconstructs_prefixes_and_indent() {
        let doc = NoteDocument {
            blocks: vec![
                blk(0, BlockKind::Heading, 0, false, 2, "Plan"),
                blk(1, BlockKind::Bullet, 0, false, 0, "groceries"),
                blk(2, BlockKind::Bullet, 1, false, 0, "buy **milk**"),
                blk(3, BlockKind::Todo, 0, false, 0, "ship it"),
                blk(4, BlockKind::Todo, 0, true, 0, "write tests"),
                blk(5, BlockKind::Ordered, 0, false, 0, "first"),
                blk(6, BlockKind::Quote, 0, false, 0, "noted"),
            ],
        };
        assert_eq!(
            serialize_note(&doc),
            "## Plan\n- groceries\n  - buy **milk**\n- [ ] ship it\n- [x] write tests\n1. first\n> noted"
        );
    }

    #[test]
    fn serialize_emits_fenced_code() {
        let doc = NoteDocument {
            blocks: vec![blk(
                0,
                BlockKind::Code,
                0,
                false,
                0,
                "let x = 1;\nlet y = 2;",
            )],
        };
        assert_eq!(serialize_note(&doc), "```\nlet x = 1;\nlet y = 2;\n```");
    }

    // ── round-trip: blocks → markdown → blocks identity (the spec's Proof 1) ──

    fn assert_roundtrips(md: &str) {
        let doc = parse_note(md);
        let doc2 = parse_note(&serialize_note(&doc));
        assert_eq!(doc, doc2, "blocks → markdown → blocks must be identity");
        // And the markdown form is stable (idempotent) once normalized.
        assert_eq!(
            serialize_note(&doc),
            serialize_note(&doc2),
            "serialized markdown must be idempotent"
        );
    }

    #[test]
    fn roundtrip_mixed_corpus() {
        assert_roundtrips(
            "# Heading\n\
             intro paragraph with **bold** and `code`\n\
             - bullet one\n\
               - nested bullet\n\
             1. ordered\n\
             - [ ] open task\n\
             - [x] done task\n\
             > a quote\n\
             ```\n\
             fn main() {}\n\
             ```\n\
             | a | b |\n\
             trailing text",
        );
    }

    #[test]
    fn roundtrip_deeply_nested_lists() {
        assert_roundtrips("- a\n  - b\n    - c\n      - d\n  - e\n- f");
    }

    #[test]
    fn roundtrip_empty_document() {
        // An empty string parses to one empty paragraph and round-trips.
        assert_roundtrips("");
    }

    // ── structural gestures ──

    /// Run a gesture and return (resulting document, resulting caret). `new_id` is a fresh id.
    fn g(
        md: &str,
        block: usize,
        offset: usize,
        gesture: StructuralGesture,
    ) -> (NoteDocument, BlockCaret) {
        let doc = parse_note(md);
        let caret = BlockCaret {
            block: doc.blocks[block].id,
            offset,
        };
        let new_id = BlockId(doc.max_id().map(|m| m.0 + 1).unwrap_or(0));
        let res = apply_structural_gesture(&doc, caret, gesture, new_id);
        (apply_edits(&doc, &res.edits), res.caret)
    }

    #[test]
    fn newline_splits_a_bullet_into_a_sibling() {
        // "- first\n- second", caret at the end of "first" (offset 5) → an empty bullet sibling.
        let (doc, caret) = g("- first\n- second", 0, 5, StructuralGesture::Newline);
        assert_eq!(
            doc.blocks,
            vec![
                blk(0, BlockKind::Bullet, 0, false, 0, "first"),
                blk(2, BlockKind::Bullet, 0, false, 0, ""),
                blk(1, BlockKind::Bullet, 0, false, 0, "second"),
            ]
        );
        assert_eq!(
            caret,
            BlockCaret {
                block: BlockId(2),
                offset: 0
            }
        );
    }

    #[test]
    fn newline_splits_text_at_the_caret() {
        // "- abcd", caret at offset 2 → "ab" stays, "cd" moves to the new sibling.
        let (doc, _) = g("- abcd", 0, 2, StructuralGesture::Newline);
        assert_eq!(doc.blocks[0].text, "ab");
        assert_eq!(doc.blocks[1].text, "cd");
    }

    #[test]
    fn newline_on_empty_nested_list_item_outdents() {
        // "- a\n  - " → the empty nested bullet outdents to depth 0 (no new block).
        let (doc, caret) = g("- a\n  - ", 1, 0, StructuralGesture::Newline);
        assert_eq!(doc.blocks.len(), 2);
        assert_eq!(doc.blocks[1], blk(1, BlockKind::Bullet, 0, false, 0, ""));
        assert_eq!(
            caret,
            BlockCaret {
                block: BlockId(1),
                offset: 0
            }
        );
    }

    #[test]
    fn newline_on_empty_top_level_list_item_exits_to_paragraph() {
        // "- " → the empty top-level bullet becomes a paragraph (exits the list).
        let (doc, caret) = g("- ", 0, 0, StructuralGesture::Newline);
        assert_eq!(
            doc.blocks,
            vec![blk(0, BlockKind::Paragraph, 0, false, 0, "")]
        );
        assert_eq!(
            caret,
            BlockCaret {
                block: BlockId(0),
                offset: 0
            }
        );
    }

    #[test]
    fn newline_in_a_paragraph_splits_into_two_paragraphs() {
        let (doc, _) = g("hello world", 0, 5, StructuralGesture::Newline);
        assert_eq!(
            doc.blocks,
            vec![
                blk(0, BlockKind::Paragraph, 0, false, 0, "hello"),
                blk(1, BlockKind::Paragraph, 0, false, 0, " world"),
            ]
        );
    }

    #[test]
    fn newline_in_a_heading_makes_the_suffix_a_paragraph() {
        // A heading does not continue: its split-off suffix is a paragraph (split_child_kind).
        let (doc, _) = g("# Title", 0, 5, StructuralGesture::Newline);
        assert_eq!(doc.blocks[0].kind, BlockKind::Heading);
        assert_eq!(doc.blocks[1].kind, BlockKind::Paragraph);
    }

    #[test]
    fn newline_split_of_a_checked_todo_yields_an_unchecked_todo() {
        // "- [x] done stuff", caret after "done" (offset 4) → new todo, unchecked.
        let (doc, _) = g("- [x] done stuff", 0, 4, StructuralGesture::Newline);
        assert_eq!(doc.blocks[0], blk(0, BlockKind::Todo, 0, true, 0, "done"));
        assert_eq!(
            doc.blocks[1],
            blk(1, BlockKind::Todo, 0, false, 0, " stuff")
        );
    }

    #[test]
    fn indent_nests_under_previous_block_and_clamps() {
        // "- a\n- b": indent b → depth 1 (under a); indenting again stays 1 (no deeper parent).
        let (doc, _) = g("- a\n- b", 1, 0, StructuralGesture::Indent);
        assert_eq!(doc.blocks[1].depth, 1);
        let (doc2, _) = g("- a\n  - b", 1, 0, StructuralGesture::Indent);
        assert_eq!(
            doc2.blocks[1].depth, 1,
            "cannot indent past previous depth + 1"
        );
    }

    #[test]
    fn indent_on_the_first_block_is_a_noop() {
        let doc = parse_note("- a\n- b");
        let caret = BlockCaret {
            block: doc.blocks[0].id,
            offset: 0,
        };
        let res = apply_structural_gesture(&doc, caret, StructuralGesture::Indent, BlockId(9));
        assert!(res.edits.is_empty());
    }

    #[test]
    fn outdent_unnests_then_noops_at_depth_zero() {
        let (doc, _) = g("- a\n  - b", 1, 0, StructuralGesture::Outdent);
        assert_eq!(doc.blocks[1].depth, 0);
        let doc0 = parse_note("- a");
        let caret = BlockCaret {
            block: doc0.blocks[0].id,
            offset: 0,
        };
        let res = apply_structural_gesture(&doc0, caret, StructuralGesture::Outdent, BlockId(9));
        assert!(res.edits.is_empty(), "outdent at depth 0 is a no-op");
    }

    #[test]
    fn toggle_checkbox_flips_a_todo_and_noops_elsewhere() {
        let (doc, _) = g("- [ ] task", 0, 0, StructuralGesture::ToggleCheckbox);
        assert!(doc.blocks[0].checked);
        let (doc2, _) = g("- [x] task", 0, 0, StructuralGesture::ToggleCheckbox);
        assert!(!doc2.blocks[0].checked);
        // A plain bullet has no checkbox.
        let bullet = parse_note("- nope");
        let caret = BlockCaret {
            block: bullet.blocks[0].id,
            offset: 0,
        };
        let res = apply_structural_gesture(
            &bullet,
            caret,
            StructuralGesture::ToggleCheckbox,
            BlockId(9),
        );
        assert!(res.edits.is_empty());
    }

    #[test]
    fn backspace_at_start_outdents_a_nested_block() {
        let (doc, caret) = g("- a\n  - b", 1, 0, StructuralGesture::BackspaceAtStart);
        assert_eq!(doc.blocks[1].depth, 0);
        assert_eq!(
            caret,
            BlockCaret {
                block: BlockId(1),
                offset: 0
            }
        );
    }

    #[test]
    fn backspace_at_start_merges_into_previous_at_depth_zero() {
        // Two paragraphs; backspace at the start of the second merges it into the first.
        let (doc, caret) = g("first\nsecond", 1, 0, StructuralGesture::BackspaceAtStart);
        assert_eq!(
            doc.blocks,
            vec![blk(0, BlockKind::Paragraph, 0, false, 0, "firstsecond")]
        );
        assert_eq!(
            caret,
            BlockCaret {
                block: BlockId(0),
                offset: 5
            }
        );
    }

    #[test]
    fn backspace_at_start_of_first_block_is_a_noop() {
        let doc = parse_note("only");
        let caret = BlockCaret {
            block: doc.blocks[0].id,
            offset: 0,
        };
        let res =
            apply_structural_gesture(&doc, caret, StructuralGesture::BackspaceAtStart, BlockId(9));
        assert!(res.edits.is_empty());
    }

    #[test]
    fn backspace_not_at_start_is_a_noop() {
        let doc = parse_note("first\nsecond");
        let caret = BlockCaret {
            block: doc.blocks[1].id,
            offset: 3,
        };
        let res =
            apply_structural_gesture(&doc, caret, StructuralGesture::BackspaceAtStart, BlockId(9));
        assert!(res.edits.is_empty());
    }

    #[test]
    fn newline_in_a_code_block_is_a_noop() {
        // Enter inside a code block inserts a literal newline (default text handling), it does
        // not split the block — the engine returns no edits so the client handles it as text.
        let doc = parse_note("```\nlet x = 1;\n```");
        let caret = BlockCaret {
            block: doc.blocks[0].id,
            offset: 5,
        };
        let res = apply_structural_gesture(&doc, caret, StructuralGesture::Newline, BlockId(9));
        assert!(res.edits.is_empty());
        assert_eq!(res.caret, caret);
    }

    #[test]
    fn gesture_on_a_missing_block_is_a_noop() {
        let doc = parse_note("- a");
        let caret = BlockCaret {
            block: BlockId(999),
            offset: 0,
        };
        let res = apply_structural_gesture(&doc, caret, StructuralGesture::Newline, BlockId(9));
        assert!(res.edits.is_empty());
        assert_eq!(res.caret, caret);
    }

    #[test]
    fn gesture_result_lowers_to_valid_markdown() {
        // End-to-end: split a bullet, lower the edits, re-serialize — the new sibling appears.
        let (doc, _) = g("- one\n- two", 0, 3, StructuralGesture::Newline);
        assert_eq!(serialize_note(&doc), "- one\n- \n- two");
    }

    // ── wire shape (pins the cross-app JSON the WASM/UniFFI faces emit; the web TS types
    //    in `apps/fauna-web/src/lib/notes.ts` mirror exactly this) ──

    #[test]
    fn wire_shape_is_stable() {
        // BlockId is a transparent newtype → a bare number.
        assert_eq!(serde_json::to_string(&BlockId(5)).unwrap(), "5");
        // Kinds + gestures are snake_case strings.
        assert_eq!(
            serde_json::to_string(&BlockKind::Bullet).unwrap(),
            "\"bullet\""
        );
        assert_eq!(
            serde_json::to_string(&StructuralGesture::BackspaceAtStart).unwrap(),
            "\"backspace_at_start\""
        );
        // A block is a flat object.
        assert_eq!(
            serde_json::to_string(&blk(0, BlockKind::Todo, 1, true, 0, "x")).unwrap(),
            r#"{"id":0,"kind":"todo","depth":1,"checked":true,"level":0,"text":"x"}"#
        );
        // BlockEdit is an externally-tagged enum (snake_case variant key → fields object).
        let split = BlockEdit::SplitBlock {
            at: BlockCaret {
                block: BlockId(0),
                offset: 2,
            },
            new_id: BlockId(9),
        };
        assert_eq!(
            serde_json::to_string(&split).unwrap(),
            r#"{"split_block":{"at":{"block":0,"offset":2},"new_id":9}}"#
        );
        let merge = BlockEdit::MergeWithPrev { block: BlockId(1) };
        assert_eq!(
            serde_json::to_string(&merge).unwrap(),
            r#"{"merge_with_prev":{"block":1}}"#
        );
        // GestureResult round-trips through JSON.
        let res = GestureResult {
            edits: vec![merge],
            caret: BlockCaret {
                block: BlockId(0),
                offset: 5,
            },
        };
        let json = serde_json::to_string(&res).unwrap();
        let back: GestureResult = serde_json::from_str(&json).unwrap();
        assert_eq!(res, back);
    }

    // ── note_line_map ────────────────────────────────────────────────────────────────────
    //
    // Golden cases ported from the proven web `noteLineMap` suite (`notes-editor.test.ts`); the
    // byte spans equal the web UTF-16 spans for ASCII corpora. The structural derivation now
    // lives here (shared by all 7 apps), so these are the cross-app oracle; web keeps only
    // the byte→UTF-16 remap of this output.

    #[test]
    fn line_map_heading_prefix_is_the_hash_run() {
        let value = "# Plan";
        let blocks = [blk(0, BlockKind::Heading, 0, false, 1, "Plan")];
        let map = note_line_map(value, &blocks);
        assert_eq!(map.len(), 1);
        assert_eq!(map[0].role, NoteLineRole::Block);
        assert_eq!((map[0].prefix_from, map[0].prefix_to), (0, 2)); // "# "
        assert_eq!((map[0].start, map[0].end), (0, 6));
        assert_eq!(map[0].block, 0);
    }

    #[test]
    fn line_map_todo_prefix_is_the_whole_checkbox_marker() {
        let value = "- [ ] ship it";
        let blocks = [blk(0, BlockKind::Todo, 0, false, 0, "ship it")];
        let map = note_line_map(value, &blocks);
        assert_eq!((map[0].prefix_from, map[0].prefix_to), (0, 6)); // "- [ ] ", not just "- "
        assert_eq!(map[0].checkbox_index, Some(0));
    }

    #[test]
    fn line_map_nested_bullet_prefix_includes_indent() {
        let value = "  - milk";
        let blocks = [blk(0, BlockKind::Bullet, 1, false, 0, "milk")];
        let map = note_line_map(value, &blocks);
        assert_eq!((map[0].prefix_from, map[0].prefix_to), (0, 4)); // "  - "
    }

    #[test]
    fn line_map_multi_block_corpus_roles_prefixes_and_checkbox_indices() {
        let value = "# Plan\n- groceries\n  - milk\n- [ ] ship it\n- [x] write tests\nBuy **milk** and run `code` now.";
        let blocks = [
            blk(0, BlockKind::Heading, 0, false, 1, "Plan"),
            blk(1, BlockKind::Bullet, 0, false, 0, "groceries"),
            blk(2, BlockKind::Bullet, 1, false, 0, "milk"),
            blk(3, BlockKind::Todo, 0, false, 0, "ship it"),
            blk(4, BlockKind::Todo, 0, true, 0, "write tests"),
            blk(
                5,
                BlockKind::Paragraph,
                0,
                false,
                0,
                "Buy **milk** and run `code` now.",
            ),
        ];
        let map = note_line_map(value, &blocks);
        assert_eq!(map.len(), 6);
        assert!(map.iter().all(|l| l.role == NoteLineRole::Block));
        assert_eq!(
            (map[3].checkbox_index, map[4].checkbox_index),
            (Some(0), Some(1))
        );
        assert_eq!((map[3].prefix_from, map[3].prefix_to), (28, 34)); // "- [ ] "
        assert_eq!((map[4].prefix_from, map[4].prefix_to), (42, 48)); // "- [x] "
        assert_eq!((map[5].prefix_from, map[5].prefix_to), (60, 60)); // paragraph — no prefix
    }

    #[test]
    fn line_map_fenced_code_spans_open_body_close() {
        let value = "```\nlet x = 1;\nlet y = 2;\n```";
        let blocks = [blk(
            0,
            BlockKind::Code,
            0,
            false,
            0,
            "let x = 1;\nlet y = 2;",
        )];
        let map = note_line_map(value, &blocks);
        let roles: Vec<_> = map.iter().map(|l| l.role).collect();
        assert_eq!(
            roles,
            vec![
                NoteLineRole::CodeOpen,
                NoteLineRole::CodeBody,
                NoteLineRole::CodeBody,
                NoteLineRole::CodeClose
            ]
        );
        assert!(map.iter().all(|l| l.block == 0));
    }

    #[test]
    fn line_map_empty_body_fenced_code_is_just_the_two_fences() {
        let value = "```\n```";
        let blocks = [blk(0, BlockKind::Code, 0, false, 0, "")];
        let map = note_line_map(value, &blocks);
        let roles: Vec<_> = map.iter().map(|l| l.role).collect();
        assert_eq!(roles, vec![NoteLineRole::CodeOpen, NoteLineRole::CodeClose]);
    }

    #[test]
    fn line_map_ordered_run_numbers_reset_by_depth_and_break() {
        let value = "1. a\n1. b\n  1. c\n1. d\n- x\n1. e";
        let blocks = [
            blk(0, BlockKind::Ordered, 0, false, 0, "a"),
            blk(1, BlockKind::Ordered, 0, false, 0, "b"),
            blk(2, BlockKind::Ordered, 1, false, 0, "c"),
            blk(3, BlockKind::Ordered, 0, false, 0, "d"),
            blk(4, BlockKind::Bullet, 0, false, 0, "x"),
            blk(5, BlockKind::Ordered, 0, false, 0, "e"),
        ];
        let map = note_line_map(value, &blocks);
        // a=1, b=2, nested c=1, d=3 (depth-0 run continues), bullet breaks it, e=1.
        let nums: Vec<_> = map.iter().map(|l| l.ordered_number).collect();
        assert_eq!(
            nums,
            vec![Some(1), Some(2), Some(1), Some(3), None, Some(1)]
        );
    }

    #[test]
    fn line_map_byte_offsets_are_utf8_not_char_counts() {
        // A multibyte char (`é` = 2 bytes) — the web UTF-16 remap relies on these being UTF-8
        // byte offsets, so prove the prefix/span math is in bytes, not code points.
        let value = "- café";
        let blocks = [blk(0, BlockKind::Bullet, 0, false, 0, "café")];
        let map = note_line_map(value, &blocks);
        assert_eq!((map[0].prefix_from, map[0].prefix_to), (0, 2)); // "- "
        assert_eq!((map[0].start, map[0].end), (0, 7)); // "- café" = 7 bytes
    }

    #[test]
    fn line_map_wire_shape_is_stable() {
        // Roles are snake_case strings; NoteLine is a flat object; Option fields → null / number.
        assert_eq!(
            serde_json::to_string(&NoteLineRole::CodeOpen).unwrap(),
            "\"code_open\""
        );
        let map = note_line_map("- [ ] x", &[blk(0, BlockKind::Todo, 0, false, 0, "x")]);
        assert_eq!(
            serde_json::to_string(&map[0]).unwrap(),
            r#"{"index":0,"start":0,"end":7,"role":"block","block":0,"prefix_from":0,"prefix_to":6,"checkbox_index":0,"ordered_number":null}"#
        );
    }

    // ── caret seam (whole-buffer byte caret ↔ BlockCaret) ──────────────────────────────────
    // Ported from the web `notes-editor.test.ts` caret round-trips (delta #5), now in the shared
    // byte unit: the platform-agnostic block lookup / prefix skip / code-body accumulation lives
    // here; web keeps only the UTF-16↔byte conversion at the seam (covered by its `utf16ToByte`
    // test). The multibyte `é` case is the same risk, proven here on raw bytes.

    #[test]
    fn caret_into_heading_content_skips_hidden_prefix() {
        let value = "# Plan";
        let blocks = parse_note(value).blocks;
        let map = note_line_map(value, &blocks);
        // "# Pl|an" — buffer byte caret 4 → offset 2 into "Plan" (past the hidden "# " prefix).
        assert_eq!(
            caret_to_block_caret(&map, &blocks, 4),
            Some(BlockCaret {
                block: BlockId(0),
                offset: 2
            })
        );
    }

    #[test]
    fn caret_round_trips_ascii() {
        let value = "- [ ] ship it";
        let blocks = parse_note(value).blocks;
        let map = note_line_map(value, &blocks);
        // prefix "- [ ] " = 6 bytes; text "ship it" = 7 bytes → buffer carets 6..=13.
        for byte in [6usize, 7, 10, 13] {
            let bc = caret_to_block_caret(&map, &blocks, byte).unwrap();
            assert_eq!(
                block_caret_to_byte(&map, &blocks, bc),
                byte,
                "round-trip @ byte {byte}"
            );
        }
    }

    #[test]
    fn caret_round_trips_multibyte() {
        // "- café": "- " = 2 bytes; "café" = 5 bytes (é = 2) → 7 bytes total. Pure byte math, so
        // the multibyte char "just works" — the UTF-16 risk stays on the web seam.
        let value = "- café";
        let blocks = parse_note(value).blocks;
        let map = note_line_map(value, &blocks);
        // Byte carets at char boundaries in/around "café": 2 (start), 3, 4, 5 (before é), 7 (end).
        for byte in [2usize, 3, 4, 5, 7] {
            let bc = caret_to_block_caret(&map, &blocks, byte).unwrap();
            assert_eq!(
                block_caret_to_byte(&map, &blocks, bc),
                byte,
                "round-trip @ byte {byte}"
            );
        }
    }

    #[test]
    fn block_caret_in_later_block_maps_to_its_line() {
        let value = "# Plan\n- milk";
        let blocks = parse_note(value).blocks;
        let map = note_line_map(value, &blocks);
        // BlockCaret{block 1, offset 2} → "- milk" line starts byte 7, prefix "- " (2) → text at
        // byte 9; +2 → byte 11 ("- mi|lk").
        assert_eq!(
            block_caret_to_byte(
                &map,
                &blocks,
                BlockCaret {
                    block: BlockId(1),
                    offset: 2
                }
            ),
            11
        );
    }

    #[test]
    fn caret_in_code_body_accumulates_bytes() {
        // A fenced block with two body lines → one `Code` block whose `text` is the body joined by
        // `\n` ("ab\ncd"). The code-body path is the trickiest byte accumulation.
        let value = "```\nab\ncd\n```";
        let blocks = parse_note(value).blocks;
        let map = note_line_map(value, &blocks);
        let id = BlockId(0);
        assert_eq!(blocks[0].text, "ab\ncd"); // body lines joined by '\n' (5 bytes)
        // Body-interior buffer carets round-trip exactly to the joined-body offset.
        for (byte, off) in [(4usize, 0usize), (5, 1), (6, 2), (7, 3), (8, 4), (9, 5)] {
            let bc = caret_to_block_caret(&map, &blocks, byte).unwrap();
            assert_eq!(
                bc,
                BlockCaret {
                    block: id,
                    offset: off
                },
                "forward @ byte {byte}"
            );
            assert_eq!(
                block_caret_to_byte(&map, &blocks, bc),
                byte,
                "round-trip @ byte {byte}"
            );
        }
        // A caret on the opening fence maps to body START (offset 0); on the closing fence to body
        // END (offset = text length) — a fence is not part of `text`.
        assert_eq!(
            caret_to_block_caret(&map, &blocks, 1),
            Some(BlockCaret {
                block: id,
                offset: 0
            })
        );
        assert_eq!(
            caret_to_block_caret(&map, &blocks, 11),
            Some(BlockCaret {
                block: id,
                offset: 5
            })
        );
    }

    #[test]
    fn caret_empty_line_map_is_none() {
        // Empty doc → no line to resolve → None (the web `caretToBlockCaret` returns null; the
        // gesture path falls through to CM's default).
        assert_eq!(caret_to_block_caret(&[], &[], 0), None);
    }

    #[test]
    fn block_caret_unknown_id_clamps_to_start() {
        let value = "# Plan";
        let blocks = parse_note(value).blocks;
        let map = note_line_map(value, &blocks);
        assert_eq!(
            block_caret_to_byte(
                &map,
                &blocks,
                BlockCaret {
                    block: BlockId(99),
                    offset: 3
                }
            ),
            0
        );
    }
}
