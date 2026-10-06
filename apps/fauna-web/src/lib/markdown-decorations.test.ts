// Deno test for the compose inline-markdown decoration mapping. Run via:
//
//     deno test apps/fauna-web/src/lib/markdown-decorations.test.ts
//
// Pure module (no wasm / DOM / CodeMirror), so it runs standalone — matching
// `notes-editor.test.ts`'s convention (plain `throw` assertions; no
// external assert dep until Vitest is wired up). The shared Rust tokenizer's
// output (`decoration_map`) is already covered by Rust unit tests; this covers
// the web-glue: UTF-8 byte → UTF-16 conversion + the per-line marker-reveal rule.

import {
  byteToUtf16Index,
  decorationRanges,
  type MdDecoration,
  type MdRevealRange,
  type DecoRange,
} from './markdown-decorations.ts';

function eq<T>(actual: T, expected: T, msg: string) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${msg}\n  expected: ${e}\n  actual:   ${a}`);
}

Deno.test('byteToUtf16Index — ASCII is identity', () => {
  const m = byteToUtf16Index('hello');
  eq(m.get(0), 0, 'byte 0');
  eq(m.get(5), 5, 'byte 5 (end)');
});

Deno.test('byteToUtf16Index — multibyte (café x): 2-byte é shifts later offsets', () => {
  // c a f é(2B) space x  → bytes 0,1,2,3,5,6,7 ; utf16 0,1,2,3,4,5,6
  const m = byteToUtf16Index('café x');
  eq(m.get(3), 3, 'byte 3 = é start');
  eq(m.get(5), 4, 'byte 5 (just past é) = utf16 4'); // mirrors linux byte_to_char test
  eq(m.get(6), 5, 'byte 6 = the x');
});

Deno.test('byteToUtf16Index — astral emoji is one code point, two UTF-16 units', () => {
  // 😀 = U+1F600 = 4 UTF-8 bytes, 2 UTF-16 code units; then x
  const m = byteToUtf16Index('😀x');
  eq(m.get(4), 2, 'byte 4 (past emoji) = utf16 2 (surrogate pair)');
  eq(m.get(5), 3, 'byte 5 = past x');
});

Deno.test('decorationRanges — empty decos → empty', () => {
  eq(decorationRanges('hello', [], []), [] as DecoRange[], 'no decorations');
});

Deno.test('decorationRanges — bold content styled; markers dimmed when caret is on another line', () => {
  // value: "*a*\nb"  bytes/utf16: * a * \n b  = 0 1 2 3 4
  // decoration_map for line 0 "*a*": Marker[0,1], Italic[1,2], Marker[2,3]
  const value = '*a*\nb';
  const decos: MdDecoration[] = [
    { start: 0, end: 1, kind: 'marker', level: 0 },
    { start: 1, end: 2, kind: 'italic', level: 0 },
    { start: 2, end: 3, kind: 'marker', level: 0 },
  ];
  // caret on line 1 → `composeShowMarkersDimRanges` dims both line-0 markers.
  const dimRanges: MdRevealRange[] = [
    { start: 0, end: 1 },
    { start: 2, end: 3 },
  ];
  eq(
    decorationRanges(value, decos, dimRanges),
    [
      { from: 0, to: 1, className: 'cm-md-marker' },
      { from: 1, to: 2, className: 'cm-md-italic' },
      { from: 2, to: 3, className: 'cm-md-marker' },
    ],
    'caret off-line → markers dimmed',
  );
});

Deno.test('decorationRanges — markers on the caret line are revealed (no decoration)', () => {
  const value = '*a*\nb';
  const decos: MdDecoration[] = [
    { start: 0, end: 1, kind: 'marker', level: 0 },
    { start: 1, end: 2, kind: 'italic', level: 0 },
    { start: 2, end: 3, kind: 'marker', level: 0 },
  ];
  // caret on line 0 (the markers' line) → `composeShowMarkersDimRanges` dims nothing;
  // markers revealed, only the content style remains.
  eq(
    decorationRanges(value, decos, []),
    [{ from: 1, to: 2, className: 'cm-md-italic' }],
    'caret on-line → markers revealed',
  );
});

Deno.test('decorationRanges — multibyte source maps the styled run to the right UTF-16 range', () => {
  // value: "é*x*"  bytes: é[0,2) *[2,3) x[3,4) *[4,5) ; utf16: é=0 *=1 x=2 *=3
  // Italic run "*x*": Marker[2,3], Italic[3,4], Marker[4,5]
  const value = 'é*x*';
  const decos: MdDecoration[] = [
    { start: 2, end: 3, kind: 'marker', level: 0 },
    { start: 3, end: 4, kind: 'italic', level: 0 },
    { start: 4, end: 5, kind: 'marker', level: 0 },
  ];
  // single line → caret is on the markers' line → nothing dimmed; the italic content
  // (byte [3,4)) must map to UTF-16 [2,3) — the `x`, NOT byte indices.
  eq(
    decorationRanges(value, decos, []),
    [{ from: 2, to: 3, className: 'cm-md-italic' }],
    'byte→utf16: italic content is the x at utf16 2',
  );
});

Deno.test('decorationRanges — heading line: dim "## " marker off-line, heading style over content, inner emphasis overlaps', () => {
  // value: "# *h*\nx"  bytes/utf16: # space * h * \n x = 0 1 2 3 4 5 6
  // decoration_map: Marker[0,2] ("# "), Heading[2,5] ("*h*"), then inline within content:
  //   Marker[2,3] ("*"), Italic[3,4] ("h"), Marker[4,5] ("*")
  const value = '# *h*\nx';
  const decos: MdDecoration[] = [
    { start: 0, end: 2, kind: 'marker', level: 0 },
    { start: 2, end: 5, kind: 'heading', level: 1 },
    { start: 2, end: 3, kind: 'marker', level: 0 },
    { start: 3, end: 4, kind: 'italic', level: 0 },
    { start: 4, end: 5, kind: 'marker', level: 0 },
  ];
  // caret on line 1 → all line-0 markers dimmed; heading + inner italic styled (overlap).
  const dimRanges: MdRevealRange[] = [
    { start: 0, end: 2 },
    { start: 2, end: 3 },
    { start: 4, end: 5 },
  ];
  eq(
    decorationRanges(value, decos, dimRanges),
    [
      { from: 0, to: 2, className: 'cm-md-marker' },
      { from: 2, to: 5, className: 'cm-md-heading' },
      { from: 2, to: 3, className: 'cm-md-marker' },
      { from: 3, to: 4, className: 'cm-md-italic' },
      { from: 4, to: 5, className: 'cm-md-marker' },
    ],
    'heading + overlapping inner emphasis, markers dimmed off-line',
  );
});

Deno.test('decorationRanges — empty range (e.g. blank "## " heading content) is dropped', () => {
  // A zero-length heading-content decoration must not produce a CM mark (from < to required).
  const decos: MdDecoration[] = [
    { start: 0, end: 3, kind: 'marker', level: 0 },
    { start: 3, end: 3, kind: 'heading', level: 2 },
  ];
  eq(
    decorationRanges('## \n', decos, [{ start: 0, end: 3 }]),
    [{ from: 0, to: 3, className: 'cm-md-marker' }],
    'empty heading content dropped',
  );
});
