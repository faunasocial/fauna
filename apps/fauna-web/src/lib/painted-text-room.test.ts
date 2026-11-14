// Deno test for `#lib/painted-text-room` — the room web's layout gave an element, convention
// 17's `painted-text-has-area` measure (`e2e-systematic-ui-walks.md` § Implementation status
// today). Run via:
//
//     deno test apps/fauna-web/src/lib/painted-text-room.test.ts
//
// The view is a fake tree, so each rule is pinned without a DOM: a clipping ancestor caps the
// box, a scroll viewport does not, an out-of-flow box escapes the clips below its containing
// block, and only shown nodes are walked.

import { type LayoutView, type NodeLayout, isWalked, paneOf, roomOf } from './painted-text-room.ts';

interface Fake {
  parent: Fake | null;
  layout: NodeLayout;
}

const view: LayoutView<Fake> = { parent: (n) => n.parent, layout: (n) => n.layout };

function node(parent: Fake | null, width: number, height: number, style: Partial<NodeLayout> = {}): Fake {
  return {
    parent,
    layout: {
      width,
      height,
      display: 'block',
      visibility: 'visible',
      position: 'static',
      overflowX: 'visible',
      overflowY: 'visible',
      containsFixed: false,
      rendered: true,
      ...style,
    },
  };
}

function eq<T>(actual: T, expected: T, msg: string) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${msg}\n  expected: ${e}\n  actual:   ${a}`);
}

const pane = () => node(null, 600, 400);

Deno.test('a box the flex layout gave no width is starved, its pane has area', () => {
  const p = pane();
  const label = node(p, 0, 18);
  eq(roomOf(label, view), { width: 0, height: 18 }, 'own box');
  eq(paneOf(label, view), { pane: { width: 600, height: 400 }, depth: 0 }, 'pane');
});

Deno.test('a clipping ancestor caps the room, per axis, and never by position', () => {
  const p = pane();
  const clipX = node(p, 0, 40, { overflowX: 'hidden' });
  const label = node(clipX, 120, 18);
  eq(roomOf(label, view), { width: 0, height: 18 }, 'overflow-x hidden caps the width only');
  const clip = node(p, 300, 0, { overflowX: 'clip', overflowY: 'clip' });
  eq(roomOf(node(clip, 120, 18), view), { width: 120, height: 0 }, 'clip caps both');
});

Deno.test('a scroll viewport is not a clip: scrolled-out content keeps its box', () => {
  const p = pane();
  const scroller = node(p, 0, 0, { overflowX: 'auto', overflowY: 'scroll' });
  eq(roomOf(node(scroller, 120, 18), view), { width: 120, height: 18 }, 'scroll');
});

Deno.test('an inline ancestor does not clip, whatever its computed overflow', () => {
  const p = pane();
  const inline = node(p, 0, 0, { display: 'inline', overflowX: 'hidden', overflowY: 'hidden' });
  eq(roomOf(node(inline, 120, 18), view), { width: 120, height: 18 }, 'inline');
});

Deno.test('an absolute box escapes clips below its containing block, not above it', () => {
  const p = pane();
  const outer = node(p, 0, 400, { overflowX: 'hidden', position: 'relative' });
  const inner = node(outer, 0, 400, { overflowX: 'hidden' });
  const menu = node(inner, 200, 18, { position: 'absolute' });
  eq(roomOf(menu, view), { width: 0, height: 18 }, 'the positioned clip still caps it');
  const free = node(node(p, 0, 400, { overflowX: 'hidden' }), 200, 18, { position: 'absolute' });
  eq(roomOf(free, view), { width: 200, height: 18 }, 'a static clip does not');
});

Deno.test('a fixed box escapes every clip below one that contains fixed boxes', () => {
  const p = pane();
  const clip = node(p, 0, 0, { overflowX: 'hidden', overflowY: 'hidden' });
  eq(roomOf(node(clip, 200, 18, { position: 'fixed' }), view), { width: 200, height: 18 }, 'fixed');
  const transformed = node(p, 0, 400, { overflowX: 'hidden', containsFixed: true });
  eq(
    roomOf(node(transformed, 200, 18, { position: 'fixed' }), view),
    { width: 0, height: 18 },
    'a transformed clip contains it',
  );
});

Deno.test('the pane skips box-less ancestors and counts them', () => {
  const p = pane();
  const contents = node(node(p, 300, 40), 0, 0, { display: 'contents' });
  eq(paneOf(node(contents, 0, 18), view), { pane: { width: 300, height: 40 }, depth: 1 }, 'depth 1');
  eq(paneOf(p, view), { pane: null, depth: 0 }, 'the root has no pane');
});

Deno.test('only shown, page-laid-out nodes with a box are walked', () => {
  const p = pane();
  eq(isWalked(node(p, 0, 18), view), true, 'a plain starved node is walked');
  eq(isWalked(node(node(p, 0, 0, { display: 'none' }), 0, 0), view), false, 'display none on the path');
  eq(isWalked(node(p, 0, 0, { visibility: 'hidden' }), view), false, 'visibility hidden');
  eq(isWalked(node(p, 0, 0, { display: 'contents' }), view), false, 'display contents has no box');
  eq(isWalked(node(node(p, 0, 0, { rendered: false }), 0, 0), view), false, 'inside a select');
});
