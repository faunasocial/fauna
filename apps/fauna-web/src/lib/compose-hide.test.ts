// Deno test for `composeMarkPlan` — the web glue that maps the shared compose marker plan
// (`fauna_core::markdown::compose_decoration_plan`, the `composeDecorationPlan` wasm face) to
// CodeMirror decorations: byte→UTF-16 conversion + content styling. Run via:
//
//     deno test apps/fauna-web/src/lib/compose-hide.test.ts
//
// The marker CLASSIFICATION (which inline markers hide, which structural prefixes dim, the
// caret-edge reveal) is shared Rust, covered by `libs/fauna-core/src/markdown.rs`
// `compose_decoration_plan` unit tests. This pins only the glue: hide → hides, dim → dimmed marks,
// content → styled marks, with byte ranges mapped through UTF-16 (incl. multibyte).
//
// Design tracked internally.

import { composeMarkPlan } from './notes-editor.ts';
import { MARKER_CLASS } from './markdown-decorations.ts';
import type { MdDecoration, ComposeMarkerPlan } from './markdown-decorations.ts';

function eq<T>(actual: T, expected: T, msg: string) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${msg}\n  expected: ${e}\n  actual:   ${a}`);
}
function deco(start: number, end: number, kind: MdDecoration['kind'], level = 0): MdDecoration {
  return { start, end, kind, level };
}

Deno.test('composeMarkPlan — hide ranges become hides; content styled; markers not double-added', () => {
  // "Buy **milk** now": content bold [6,10]; the plan hides the ** at [4,6]+[10,12].
  const value = 'Buy **milk** now';
  const decos = [deco(4, 6, 'marker'), deco(6, 10, 'bold'), deco(10, 12, 'marker')];
  const plan: ComposeMarkerPlan = { hide: [{ start: 4, end: 6 }, { start: 10, end: 12 }], dim: [] };
  const out = composeMarkPlan(value, decos, plan);
  eq(out.hides, [{ from: 4, to: 6 }, { from: 10, to: 12 }], 'hide ranges → hides');
  eq(out.marks, [{ from: 6, to: 10, className: 'cm-md-bold' }], 'bold content only (markers skipped)');
});

Deno.test('composeMarkPlan — dim ranges become MARKER_CLASS marks', () => {
  const value = '# Plan';
  const decos = [deco(0, 2, 'marker'), deco(2, 6, 'heading', 1)];
  const plan: ComposeMarkerPlan = { hide: [], dim: [{ start: 0, end: 2 }] };
  const out = composeMarkPlan(value, decos, plan);
  eq(out.hides, [], 'nothing hidden');
  eq(
    out.marks,
    [
      { from: 2, to: 6, className: 'cm-md-heading' },
      { from: 0, to: 2, className: MARKER_CLASS },
    ],
    'heading content styled + the "# " prefix dimmed',
  );
});

Deno.test('composeMarkPlan — byte ranges map through UTF-16 for multibyte content', () => {
  // "é*x*": é is 2 bytes / 1 UTF-16 unit, so byte offsets 2,3,4,5 → UTF-16 1,2,3,4.
  const value = 'é*x*';
  const decos = [deco(2, 3, 'marker'), deco(3, 4, 'italic'), deco(4, 5, 'marker')];
  const plan: ComposeMarkerPlan = { hide: [{ start: 2, end: 3 }, { start: 4, end: 5 }], dim: [] };
  const out = composeMarkPlan(value, decos, plan);
  eq(out.hides, [{ from: 1, to: 2 }, { from: 3, to: 4 }], 'marker byte ranges → UTF-16');
  eq(out.marks, [{ from: 2, to: 3, className: 'cm-md-italic' }], 'italic content byte [3,4] → UTF-16 [2,3]');
});
