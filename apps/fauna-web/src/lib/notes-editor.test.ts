// Deno test for the Notes WYSIWYG editor's pure decoration + caret layer. Run via:
//
//     deno test apps/fauna-web/src/lib/notes-editor.test.ts
//
// Pure module (no wasm / DOM / CodeMirror), matching `markdown-decorations.test.ts`'s
// convention (plain `throw` assertions; decos/blocks/reveal hand-fed — the shared Rust
// `decoration_map` / `inline_reveal_ranges` / `parse_note` are covered by Rust unit tests). This
// covers the web glue that is the real risk: the structural-vs-inline marker split, the
// hide/reveal decision, and the UTF-16↔UTF-8 caret arithmetic (delta #5).
//
// The hand-fed `decos` mirror `decoration_map`'s exact output (verified against
// libs/fauna-core/src/markdown.rs:797-925): a heading/quote/list/fence prefix marker is a
// SUBSET of the `parseNote` structural prefix (e.g. `- [ ] ` → `decoration_map` `ListMarker[0,2]`
// for `- `, while `parseNote`'s prefix is the whole `- [ ] `), which is exactly why the split
// works.

import {
  mapNoteLines,
  computeNotePlan,
  utf16ToByte,
  byteToUtf16,
  lineAt,
  type NotePlan,
} from './notes-editor.ts';
import type { Block, WasmNoteLine } from './notes.ts';
import type { MdDecoration, MdRevealRange } from './markdown-decorations.ts';

function eq<T>(actual: T, expected: T, msg: string) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${msg}\n  expected: ${e}\n  actual:   ${a}`);
}

function block(p: Partial<Block> & Pick<Block, 'id' | 'kind' | 'text'>): Block {
  return { depth: 0, checked: false, level: 0, ...p };
}
function deco(start: number, end: number, kind: MdDecoration['kind'], level = 0): MdDecoration {
  return { start, end, kind, level };
}
// Build a byte-based `noteLineMap` wasm-shape line (the structural projection now produced by
// shared Rust — `fauna_core::notes::note_line_map`, covered by `notes::tests::line_map_*`). The
// web tests hand-feed these (like `decos`) so this module stays pure (no wasm); `mapNoteLines`
// does the byte→UTF-16 remap that is the web-specific risk under test. Offsets are BYTES.
function rl(
  p: Partial<WasmNoteLine> & Pick<WasmNoteLine, 'index' | 'start' | 'end' | 'block'>,
): WasmNoteLine {
  return {
    role: 'block',
    prefix_from: p.start,
    prefix_to: p.start,
    checkbox_index: null,
    ordered_number: null,
    ...p,
  };
}

// ── mapNoteLines (byte→UTF-16 remap of the shared structural projection) ───────────────────────
// The structural derivation (roles, code folding, checkbox/ordered numbering, prefix range) moved
// to shared Rust (`fauna_core::notes::note_line_map`, covered by `notes::tests::line_map_*`);
// these cover the web-only remap — the byte→UTF-16 offset math (the real risk, now exercised on a
// multibyte char the old ASCII tests never hit) + the null→sentinel mapping + block re-attach.

Deno.test('mapNoteLines — multibyte: byte offsets become UTF-16 (é = 2 bytes, 1 unit)', () => {
  const value = '- café'; // bytes: "- "=2 + "café"=5 (é=2) → 7; UTF-16: "- café" = 6 units
  const blocks = [block({ id: 0, kind: 'bullet', text: 'café' })];
  const map = mapNoteLines(value, [rl({ index: 0, start: 0, end: 7, block: 0, prefix_to: 2 })], blocks);
  eq([map[0].start, map[0].end], [0, 6], 'end remapped to UTF-16 (6), not byte (7)');
  eq([map[0].prefixFrom, map[0].prefixTo], [0, 2], '"- " prefix (ASCII, unchanged)');
  eq(map[0].block.text, 'café', 'block re-attached by index');
});

Deno.test('mapNoteLines — null checkbox/ordered → -1/0 sentinels; values pass through', () => {
  const value = '- [ ] a\n1. b';
  const blocks = [block({ id: 0, kind: 'todo', text: 'a' }), block({ id: 1, kind: 'ordered', text: 'b' })];
  const map = mapNoteLines(value, [
    rl({ index: 0, start: 0, end: 7, block: 0, prefix_to: 6, checkbox_index: 0 }),
    rl({ index: 1, start: 8, end: 12, block: 1, prefix_to: 11, ordered_number: 1 }),
  ], blocks);
  eq([map[0].checkboxIndex, map[0].orderedNumber], [0, 0], 'todo: checkbox 0, ordered sentinel 0');
  eq([map[1].checkboxIndex, map[1].orderedNumber], [-1, 1], 'ordered: checkbox sentinel -1, number 1');
});

Deno.test('mapNoteLines — out-of-range block index → paragraph fallback (no crash)', () => {
  const map = mapNoteLines('x', [rl({ index: 0, start: 0, end: 1, block: 4294967295 })], []);
  eq(map[0].block.id, -1, 'fallback block for a short blocks slice');
});

Deno.test('mapNoteLines — role passes through (code roles)', () => {
  const value = '```\nx\n```';
  const blocks = [block({ id: 0, kind: 'code', text: 'x' })];
  const map = mapNoteLines(value, [
    rl({ index: 0, start: 0, end: 3, block: 0, role: 'code_open' }),
    rl({ index: 1, start: 4, end: 5, block: 0, role: 'code_body' }),
    rl({ index: 2, start: 6, end: 9, block: 0, role: 'code_close' }),
  ], blocks);
  eq(map.map((l) => l.role), ['code_open', 'code_body', 'code_close'], 'roles preserved');
});

// ── computeNotePlan — structural chrome (delta #4) ──────────────────────────────────────────

Deno.test('computeNotePlan — heading: prefix hidden (no widget), no inline marks', () => {
  const value = '# Plan';
  const blocks = [block({ id: 0, kind: 'heading', level: 1, text: 'Plan' })];
  const lineMap = mapNoteLines(value, [rl({ index: 0, start: 0, end: 6, block: 0, prefix_to: 2 })], blocks);
  const decos = [deco(0, 2, 'marker'), deco(2, 6, 'heading', 1)];
  const plan = computeNotePlan(value, lineMap, decos, []);
  eq(plan.lines, [{ pos: 0, className: 'cm-note-heading cm-note-heading-1', depth: 0 }], 'line class');
  eq(plan.hides, [{ from: 0, to: 2, widget: undefined }], 'prefix hidden, no glyph');
  eq(plan.marks, [], 'heading content styled by line class, no inline mark');
});

Deno.test('computeNotePlan — bullet: prefix hidden + bullet widget', () => {
  const value = '- groceries';
  const blocks = [block({ id: 0, kind: 'bullet', text: 'groceries' })];
  const lineMap = mapNoteLines(value, [rl({ index: 0, start: 0, end: 11, block: 0, prefix_to: 2 })], blocks);
  const decos = [deco(0, 2, 'list_marker')];
  const plan = computeNotePlan(value, lineMap, decos, []);
  eq(plan.hides, [{ from: 0, to: 2, widget: { kind: 'bullet' } }], 'bullet glyph over "- "');
  eq(plan.lines[0].className, 'cm-note-list', 'list line class');
});

Deno.test('computeNotePlan — todo: "- [ ] " hidden + checkbox widget (indexed), content shown', () => {
  const value = '- [ ] ship it';
  const blocks = [block({ id: 0, kind: 'todo', text: 'ship it' })];
  const lineMap = mapNoteLines(value,
    [rl({ index: 0, start: 0, end: 13, block: 0, prefix_to: 6, checkbox_index: 0 })], blocks);
  // decoration_map only marks "- " (ListMarker[0,2]); "[ ] " scans as plain (no inline deco).
  const decos = [deco(0, 2, 'list_marker')];
  const plan = computeNotePlan(value, lineMap, decos, []);
  eq(plan.hides, [
    { from: 0, to: 6, widget: { kind: 'checkbox', checked: false, index: 0, blockId: 0 } },
  ], 'whole "- [ ] " replaced by an unchecked checkbox');
  eq(plan.marks, [], 'no stray "[ ]" mark — covered by the structural replace');
});

Deno.test('computeNotePlan — checked todo carries checked:true', () => {
  const value = '- [x] write tests';
  const blocks = [block({ id: 0, kind: 'todo', checked: true, text: 'write tests' })];
  const lineMap = mapNoteLines(value,
    [rl({ index: 0, start: 0, end: 17, block: 0, prefix_to: 6, checkbox_index: 0 })], blocks);
  const plan = computeNotePlan(value, lineMap, [deco(0, 2, 'list_marker')], []);
  eq((plan.hides[0].widget as { checked: boolean }).checked, true, 'checked checkbox');
});

// ── computeNotePlan — inline conceal + caret-edge reveal (delta #3) ──────────────────────────

const PARA = 'Buy **milk** and run `code` now.';
const PARA_BLOCKS = [block({ id: 0, kind: 'paragraph', text: PARA })];
// One paragraph line (ASCII, 31 bytes), no structural prefix.
const PARA_LINES = mapNoteLines(PARA, [rl({ index: 0, start: 0, end: 31, block: 0 })], PARA_BLOCKS);
// decoration_map(PARA): bold "**milk**" → Marker[4,6] Bold[6,10] Marker[10,12]; code "`code`" →
// Marker[21,22] Code[22,26] Marker[26,27].
const PARA_DECOS = [
  deco(4, 6, 'marker'), deco(6, 10, 'bold'), deco(10, 12, 'marker'),
  deco(21, 22, 'marker'), deco(22, 26, 'code'), deco(26, 27, 'marker'),
];

Deno.test('computeNotePlan — caret away: every inline marker hidden, content styled', () => {
  const plan = computeNotePlan(PARA, PARA_LINES, PARA_DECOS, []);
  eq(plan.hides, [{ from: 4, to: 6 }, { from: 10, to: 12 }, { from: 21, to: 22 }, { from: 26, to: 27 }],
    'all 4 markers atomically hidden');
  eq(plan.marks, [{ from: 6, to: 10, className: 'cm-md-bold' }, { from: 22, to: 26, className: 'cm-md-code' }],
    'bold + code content styled');
});

Deno.test('computeNotePlan — caret in bold run: its ** reveals (dim), code ` stays hidden (LOCAL)', () => {
  // inline_reveal_ranges(PARA, 8) → the bold run's markers (bytes [4,6] and [10,12]).
  const reveal: MdRevealRange[] = [{ start: 4, end: 6 }, { start: 10, end: 12 }];
  const plan = computeNotePlan(PARA, PARA_LINES, PARA_DECOS, reveal);
  // The bold markers move from `hides` to dim `marks`; the code markers stay hidden.
  eq(plan.hides, [{ from: 21, to: 22 }, { from: 26, to: 27 }], 'only code markers still hidden');
  const dimmed = plan.marks.filter((m) => m.className === 'cm-md-marker');
  eq(dimmed, [{ from: 4, to: 6, className: 'cm-md-marker' }, { from: 10, to: 12, className: 'cm-md-marker' }],
    'bold markers revealed as editable dim');
});

Deno.test('computeNotePlan — empty marker range is dropped (invalid for a CM range)', () => {
  const blocks = [block({ id: 0, kind: 'paragraph', text: 'x' })];
  const lineMap = mapNoteLines('x', [rl({ index: 0, start: 0, end: 1, block: 0 })], blocks);
  const plan = computeNotePlan('x', lineMap, [deco(0, 0, 'marker')], []);
  eq(plan.hides, [], 'no zero-width hide');
});

Deno.test('computeNotePlan — fenced code body: styled by line class, fence markers not hidden inline', () => {
  const value = ['```', 'let x = 1;', '```'].join('\n');
  const blocks = [block({ id: 0, kind: 'code', text: 'let x = 1;' })];
  const lineMap = mapNoteLines(value, [
    rl({ index: 0, start: 0, end: 3, block: 0, role: 'code_open' }),
    rl({ index: 1, start: 4, end: 14, block: 0, role: 'code_body' }),
    rl({ index: 2, start: 15, end: 18, block: 0, role: 'code_close' }),
  ], blocks);
  // decoration_map: Marker over each fence line + Code over the body line.
  const decos = [deco(0, 3, 'marker'), deco(4, 14, 'code'), deco(15, 18, 'marker')];
  const plan = computeNotePlan(value, lineMap, decos, []);
  eq(plan.hides, [], 'no inline hides on code lines (fences handled by the code line class)');
  eq(plan.marks, [], 'no inline content mark on a code body (line class styles it)');
  eq(plan.lines.map((l) => l.className),
    ['cm-note-code cm-note-code-fence', 'cm-note-code', 'cm-note-code cm-note-code-fence'], 'code classes');
});

// ── lineAt ──────────────────────────────────────────────────────────────────────────────────

Deno.test('lineAt — finds the line containing an offset; line-end belongs to that line', () => {
  const value = 'ab\ncd';
  const blocks = [block({ id: 0, kind: 'paragraph', text: 'ab' }), block({ id: 1, kind: 'paragraph', text: 'cd' })];
  const map = mapNoteLines(value, [
    rl({ index: 0, start: 0, end: 2, block: 0 }),
    rl({ index: 1, start: 3, end: 5, block: 1 }),
  ], blocks);
  eq(lineAt(map, 0)!.index, 0, 'start of line 0');
  eq(lineAt(map, 2)!.index, 0, 'end of line 0 (before \\n) → line 0');
  eq(lineAt(map, 3)!.index, 1, 'start of line 1');
});

// ── caret seam: UTF-16↔byte conversion (delta #5) ────────────────────────────────────────────
// The block lookup / structural-prefix skip / code-body byte accumulation moved to shared Rust
// (`fauna_core::notes::{caret_to_block_caret, block_caret_to_byte}`, covered by
// `notes::tests::caret_*` incl. the round-trips). All web keeps is the UTF-16↔byte conversion the
// CM wiring composes around the wasm caret faces — a Rust `&str` has no UTF-16 offset, so this is
// inherently web glue. These two pure helpers are that seam.

Deno.test('utf16ToByte — multibyte: é is 2 bytes, 1 UTF-16 unit', () => {
  eq(utf16ToByte('café', 4), 5, 'whole "café" = 5 bytes');
  eq(utf16ToByte('café', 3), 3, 'before é = 3 bytes');
});

Deno.test('byteToUtf16 — inverse of utf16ToByte on the whole buffer; multibyte boundary + clamp', () => {
  const value = '- café'; // 7 bytes, 6 UTF-16 units (é = 2 bytes, 1 unit)
  eq(byteToUtf16(value, 2), 2, '"- " = 2 bytes = 2 units');
  eq(byteToUtf16(value, 5), 5, 'before é (byte 5) → unit 5');
  eq(byteToUtf16(value, 7), 6, 'after é (byte 7) → unit 6');
  eq(byteToUtf16(value, 99), 6, 'past-the-end clamps to the UTF-16 length');
});

Deno.test('utf16ToByte ∘ byteToUtf16 — round-trips at every UTF-16 boundary (multibyte)', () => {
  const value = '- café';
  for (const u16 of [0, 1, 2, 3, 4, 5, 6]) {
    eq(byteToUtf16(value, utf16ToByte(value, u16)), u16, `round-trip @ u16 ${u16}`);
  }
});
