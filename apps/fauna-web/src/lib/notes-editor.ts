// The shared editor decoration + caret layer (web glue, no CodeMirror / DOM / wasm) for BOTH
// the Notes WYSIWYG editor AND the conversations compose field's hide-by-default mode (the two
// share one inline-marker hide/reveal engine — priority #2/#4; design
// tracked internally). `computeNotePlan` is the full Notes
// plan (inline hide + structural chrome + gestures, via `notes-editor-cm.ts`); `computeComposePlan`
// is the inline-only variant for compose (inline markers hidden, structural markers left DIMMED —
// no chrome, no gestures, Enter still sends). The Notes WYSIWYG editor design backing the structural
// half is the browser realization of the Notes WYSIWYG editor design (tracked internally): a
// **mode** on the
// shared `MarkdownEditor.svelte` (NOT a fork — priority #4) that renders a markdown buffer
// **bullets-first, markers-NEVER-shown** by combining two shared-Rust inputs the conversations
// editor already uses, plus the Notes block model:
//
//   - **Structural chrome** (block-level) is drawn from the shared `parseNote` block model
//     (`$lib/notes`) — each buffer line's `kind`/`depth`/`checked`/`level` decides a line class
//     (heading size / quote bar / list / code) + a replace-widget over the structural prefix
//     (bullet glyph / number / checkbox). The prefix (`- `, `# `, `> `, `1. `, `- [ ] `,
//     indentation) is HIDDEN — it is never editable source, it is chrome (spec § linchpin).
//   - **Inline conceal** (within-block) is the shipped `decoration_map` marker ranges, but with
//     a **Hide** treatment (atomic `Decoration.replace`) instead of conversations' **Dim**, and
//     a **per-run caret-edge reveal** driven by the shared `inline_reveal_ranges` set (un-hide the
//     `**`/`*`/`` ` ``/`[]()` of the run the caret is in so it can be edited) instead of
//     conversations' whole-line reveal.
//
// Everything here is pure (inputs are the wasm outputs, hand-fed in `notes-editor.test.ts`), so
// the offset arithmetic that is the real risk is unit-testable without a browser. The CM wiring
// (widgets, atomic ranges, the gesture keymap) lives in `notes-editor-cm.ts`; the conversations
// applier stays in `markdown-decorations.ts`, untouched.

import {
  byteToUtf16Index,
  utf8Len,
  MARKER_CLASS,
  CONTENT_CLASS,
  type MdDecoration,
  type MdRevealRange,
  type DecoRange,
  type ComposeMarkerPlan,
} from './markdown-decorations.ts';
import type { Block, BlockKind, NoteLineRole, WasmNoteLine } from './notes.ts';

/** Per-buffer-line structural info, in UTF-16 (CodeMirror) units. One entry per buffer line. */
export interface NoteLine {
  /** 0-based buffer line index. */
  index: number;
  /** UTF-16 offset of the line's first char in the buffer. */
  start: number;
  /** UTF-16 offset of the line's end (the position before its `\n`, or buffer end). */
  end: number;
  role: NoteLineRole;
  /** The block this line belongs to. */
  block: Block;
  /** Structural-prefix range to hide + replace with chrome, UTF-16 (`from === to` ⇒ none —
   *  paragraph / raw / code lines carry no editable-derived prefix). */
  prefixFrom: number;
  prefixTo: number;
  /** 0-based index among the document's `todo` blocks (the indexed `document-checkbox-{n}` id),
   *  or -1 when the line is not a todo. */
  checkboxIndex: number;
  /** 1-based number within the consecutive ordered-list run at this line's depth, or 0 when the
   *  line is not an ordered item. (Markdown renumbers from 1, so this is display-only.) */
  orderedNumber: number;
}

/** A chrome widget that replaces a hidden structural prefix. */
export type NoteWidget =
  | { kind: 'bullet' }
  | { kind: 'ordered'; number: number }
  | { kind: 'checkbox'; checked: boolean; index: number; blockId: number };

/** One atomic hide range (`Decoration.replace`), UTF-16. `widget` present ⇒ replace with chrome
 *  (a structural prefix); absent ⇒ a pure hide (an inline marker the caret is away from). */
export interface NoteHide {
  from: number;
  to: number;
  widget?: NoteWidget;
}

/** One per-line line decoration (`Decoration.line`): a class + the `depth` the wiring turns into
 *  a left indent. */
export interface NoteLineDeco {
  /** UTF-16 offset of the line start (`Decoration.line` anchors at a line boundary). */
  pos: number;
  className: string;
  depth: number;
}

/** The full Notes decoration plan for one (buffer, caret) — everything the CM wiring needs. */
export interface NotePlan {
  /** `Decoration.mark` ranges: inline content styles + revealed (dim, editable) inline markers. */
  marks: DecoRange[];
  /** Atomic `Decoration.replace` ranges: structural prefixes (with chrome widget) + hidden inline
   *  markers (no widget). The wiring feeds these to `EditorView.atomicRanges` so the caret skips a
   *  hidden run as one unit. */
  hides: NoteHide[];
  /** Per-line `Decoration.line` decorations. */
  lines: NoteLineDeco[];
}

const LIST_KINDS: ReadonlySet<BlockKind> = new Set(['bullet', 'ordered', 'todo']);

/** Line class for a block line by kind/level/role. Block-level styling (heading size, quote bar,
 *  list, code background) is driven by these classes, so the inline path never emits a
 *  heading/blockquote content mark (avoids double-styling). */
function lineClass(block: Block, role: NoteLineRole): string {
  if (role !== 'block') {
    return role === 'code_body' ? 'cm-note-code' : 'cm-note-code cm-note-code-fence';
  }
  switch (block.kind) {
    case 'heading': {
      const level = Math.min(Math.max(block.level, 1), 4);
      return `cm-note-heading cm-note-heading-${level}`;
    }
    case 'quote':
      return 'cm-note-quote';
    case 'bullet':
    case 'ordered':
    case 'todo':
      return 'cm-note-list';
    case 'code':
      return 'cm-note-code';
    default:
      return 'cm-note-paragraph';
  }
}

/** The paragraph fallback for the (unreachable) case of a `blocks` slice shorter than the
 *  buffer's logical lines — mirrors the shared `note_line_map`'s `usize::MAX` block sentinel. */
const FALLBACK_BLOCK: Block = { id: -1, kind: 'paragraph', depth: 0, checked: false, level: 0, text: '' };

/**
 * Remap the shared byte-based `noteLineMap` output (`fauna_core::notes::note_line_map`, via the
 * `noteLineMap` wasm face) to UTF-16 `NoteLine`s for CodeMirror.
 *
 * **Pure** — the structural derivation (code folding, checkbox indexing, ordered-list numbering,
 * structural-prefix range) is shared Rust, computed once for all 7 apps (priority #2); this is
 * only the byte→UTF-16 offset remap + block re-attach. The UTF-16 offsets the rest of this module
 * needs come from `byteToUtf16Index` (the same seam the inline-decoration path uses), so the
 * offset arithmetic — the real risk — is unit-testable without a browser (`notes-editor.test.ts`).
 * `rustLines` / `blocks` are the wasm `noteLineMap(value, blocks)` / `parseNote(value).blocks`
 * outputs (the CM wiring calls wasm; this stays pure so the tests do too).
 */
export function mapNoteLines(value: string, rustLines: WasmNoteLine[], blocks: Block[]): NoteLine[] {
  const idx = byteToUtf16Index(value);
  const b2u = (b: number): number => idx.get(b) ?? value.length;
  return rustLines.map((rl) => ({
    index: rl.index,
    start: b2u(rl.start),
    end: b2u(rl.end),
    role: rl.role,
    block: blocks[rl.block] ?? FALLBACK_BLOCK,
    prefixFrom: b2u(rl.prefix_from),
    prefixTo: b2u(rl.prefix_to),
    checkboxIndex: rl.checkbox_index ?? -1,
    orderedNumber: rl.ordered_number ?? 0,
  }));
}

/** The `NoteLine` containing UTF-16 offset `u16` (a line covers `[start, end]`; the position at
 *  a line's `end` belongs to that line, not the next — matches a caret sitting at line end). */
export function lineAt(lineMap: NoteLine[], u16: number): NoteLine | null {
  for (const l of lineMap) {
    if (u16 >= l.start && u16 <= l.end) return l;
  }
  return lineMap.length > 0 ? lineMap[lineMap.length - 1] : null;
}

function widgetFor(line: NoteLine): NoteWidget | undefined {
  if (line.role !== 'block') return undefined;
  switch (line.block.kind) {
    case 'bullet':
      return { kind: 'bullet' };
    case 'ordered':
      return { kind: 'ordered', number: line.orderedNumber };
    case 'todo':
      return {
        kind: 'checkbox',
        checked: line.block.checked,
        index: line.checkboxIndex,
        blockId: line.block.id,
      };
    default:
      return undefined; // heading `# ` / quote `> ` hide with no glyph (line class styles them)
  }
}

/**
 * Compute the full Notes decoration plan for `(value, caret)`.
 *
 * @param value the markdown buffer (the CM document — source of truth, markers PRESENT).
 * @param lineMap `mapNoteLines(value, noteLineMap(value, blocks), blocks)` — the per-line
 *   structural projection (shared Rust derivation, byte→UTF-16-remapped). Passed in (not built
 *   here) so this stays a pure function the tests exercise without wasm.
 * @param decos `decorationMap(value)` — byte ranges of every inline + structural marker + content.
 * @param revealRanges `inlineRevealRanges(value, caretByteOffset)` — inline markers to reveal.
 *
 * The caret is not a parameter — it is already encoded in `revealRanges` (the caller computes
 * `inlineRevealRanges(value, caretByteOffset)`), so the reveal decision here is the shared set,
 * never a re-derived JS predicate (the whole point of delta #1 — priority #2).
 */
export function computeNotePlan(
  value: string,
  lineMap: NoteLine[],
  decos: MdDecoration[],
  revealRanges: MdRevealRange[],
): NotePlan {
  const idx = byteToUtf16Index(value);
  const b2u = (b: number): number => idx.get(b) ?? value.length;

  const lines: NoteLineDeco[] = lineMap.map((l) => ({
    pos: l.start,
    className: lineClass(l.block, l.role),
    depth: l.block.depth,
  }));

  const hides: NoteHide[] = [];
  // Structural prefixes → atomic hide + chrome widget.
  for (const l of lineMap) {
    if (l.role !== 'block') continue;
    if (l.prefixTo > l.prefixFrom) {
      hides.push({ from: l.prefixFrom, to: l.prefixTo, widget: widgetFor(l) });
    }
  }

  // Inline markers + content styles. Structural markers (heading/quote/fence/list prefixes) are
  // EXCLUDED here — chrome above already handles them; the inline path only touches the
  // within-content `**`/`*`/`` ` ``/`[]()` markers.
  const reveals = revealRanges.map((r) => ({ from: b2u(r.start), to: b2u(r.end) }));
  const isRevealed = (from: number, to: number): boolean =>
    reveals.some((r) => r.from <= from && to <= r.to);

  const marks: DecoRange[] = [];
  for (const d of decos) {
    const from = b2u(d.start);
    const to = b2u(d.end);
    if (from >= to) continue; // empty range — invalid for a CM mark/replace
    const line = lineAt(lineMap, from);
    const onCodeLine = line !== null && line.role !== 'block';

    if (d.kind === 'marker' || d.kind === 'list_marker') {
      if (onCodeLine) continue; // fenced ``` marker — handled by the code line class
      if (line !== null && from >= line.prefixFrom && to <= line.prefixTo) continue; // structural prefix
      // An inline emphasis marker: reveal (dim, editable) when the caret is in its run, else hide.
      if (isRevealed(from, to)) marks.push({ from, to, className: MARKER_CLASS });
      else hides.push({ from, to });
      continue;
    }

    // Content kinds. Block-level kinds (heading/blockquote) are styled by the line class, and
    // fenced-code content by the code line class — so only emit INLINE content marks.
    if (d.kind === 'heading' || d.kind === 'blockquote') continue;
    if (onCodeLine) continue;
    const cls = CONTENT_CLASS[d.kind];
    if (cls) marks.push({ from, to, className: cls });
  }

  return { marks, hides, lines };
}

/** The compose-field decoration plan for one (buffer, caret) — the inline-only subset of
 *  `NotePlan` (no chrome widgets, no line decorations, no gestures). */
export interface ComposePlan {
  /** `Decoration.mark` ranges: inline + heading/blockquote content styles, plus DIMMED markers
   *  (structural prefixes off the caret line; inline emphasis markers revealed at the caret edge). */
  marks: DecoRange[];
  /** Atomic `Decoration.replace` ranges: hidden inline emphasis markers (the caret skips them as
   *  one unit; un-hide — leave this set — the moment the caret reaches the run, like Notes). */
  hides: { from: number; to: number }[];
}

/**
 * Apply the shared compose-field marker plan to the buffer — the hide-by-default mode
 * (design tracked internally). The classification (which inline
 * markers hide, which structural prefixes dim, the caret-edge reveal) lives in **shared Rust**
 * (`fauna_core::markdown::compose_decoration_plan`, via the `composeDecorationPlan` wasm face — one
 * policy for web + native, priority #2); this is pure glue: convert the plan's byte ranges to
 * UTF-16 and style content from `decos`.
 *
 * - `plan.hide` → atomic `Decoration.replace` (inline markers concealed; the caret skips them).
 * - `plan.dim` → dim `cm-md-marker` (structural prefixes off the caret line + revealed inline).
 * - Content (`decos` non-marker kinds) → styled (`cm-md-*`), as compose does today.
 */
export function composeMarkPlan(
  value: string,
  decos: MdDecoration[],
  plan: ComposeMarkerPlan,
): ComposePlan {
  const idx = byteToUtf16Index(value);
  const b2u = (b: number): number => idx.get(b) ?? value.length;

  const marks: DecoRange[] = [];
  const hides: { from: number; to: number }[] = [];

  // Content styles — the plan handles the markers, so skip marker kinds here.
  for (const d of decos) {
    if (d.kind === 'marker' || d.kind === 'list_marker') continue;
    const from = b2u(d.start);
    const to = b2u(d.end);
    if (from >= to) continue;
    const cls = CONTENT_CLASS[d.kind];
    if (cls) marks.push({ from, to, className: cls });
  }
  for (const r of plan.dim) {
    const from = b2u(r.start);
    const to = b2u(r.end);
    if (from < to) marks.push({ from, to, className: MARKER_CLASS });
  }
  for (const r of plan.hide) {
    const from = b2u(r.start);
    const to = b2u(r.end);
    if (from < to) hides.push({ from, to });
  }

  return { marks, hides };
}

// ── Caret seam: CodeMirror UTF-16 whole-buffer caret ↔ UTF-8 byte offset ─────────────────────
//
// A gesture is computed by the shared `apply_structural_gesture` over a `BlockCaret` (a block id +
// a UTF-8 byte offset into that block's `text`). The CM caret is a UTF-16 offset into the whole
// buffer. The block lookup / structural-prefix skip / code-body byte accumulation that bridges the
// two is **shared Rust** (`fauna_core::notes::{caret_to_block_caret, block_caret_to_byte}`, via the
// `caretToBlockCaret` / `blockCaretToByte` wasm faces — one policy for all 7 apps, priority #2).
// All web keeps is the platform-glue UTF-16↔byte conversion below: a Rust `&str` has no UTF-16
// offset, so this is inherently web-only and stays here (unit-tested without wasm). The CM wiring
// (`notes-editor-cm.ts`) composes these around the wasm caret faces.

/** UTF-8 byte offset of whole-buffer UTF-16 offset `u16` within `text` (the inverse of
 *  `byteToUtf16Index`, walked by code point; clamps a past-the-end `u16` to the byte length). */
export function utf16ToByte(text: string, u16: number): number {
  let i = 0;
  let byte = 0;
  for (const ch of text) {
    if (i >= u16) return byte;
    i += ch.length;
    byte += utf8Len(ch.codePointAt(0)!);
  }
  return byte;
}

/** Whole-buffer UTF-16 offset of UTF-8 byte offset `byte` in `value` (the inverse of
 *  `utf16ToByte`; clamps a non-boundary / past-the-end `byte` to the buffer's UTF-16 length). */
export function byteToUtf16(value: string, byte: number): number {
  return byteToUtf16Index(value).get(byte) ?? value.length;
}
