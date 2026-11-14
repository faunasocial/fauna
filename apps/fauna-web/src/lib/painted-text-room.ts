// The room web's layout gave an element: convention 17's `painted-text-has-area`
// measure, as pure functions over a small view of the layout, so a deno test can pin
// it without a DOM. `#lib/e2e-painted-text-area` adapts the live DOM to the view and
// feeds the shared `fauna_e2e_contract::PaintedTextArea`, which keeps the starved
// entries; `tests/e2e-unified/helpers/frame_invariants.py::
// _check_painted_text_has_area` owns the contract and the verdict.
//
// **The ROOM, never the box alone, and sizes, never positions** (the windows leg's
// two refuted readings, `e2e-systematic-ui-walks.md` § Implementation status today):
// an element's box is capped in each dimension by the box of every ancestor that
// clips it — `overflow: hidden`/`clip` — while a scroll container (`auto`/`scroll`)
// is a viewport, not a clip, so content scrolled out of view keeps its box. Only
// sizes are compared, never the intersection of positions, so the measure can miss
// a starved element but never invent one (an element merely placed outside a
// clipping ancestor is not starved by it).
//
// Test bundles only: the sole importer is `#lib/e2e-painted-text-area`.

/** What the measure reads of one node. `width`/`height` are its border box in CSS
 *  px (`getBoundingClientRect()`); the strings are its computed style. */
export interface NodeLayout {
  width: number;
  height: number;
  display: string;
  visibility: string;
  position: string;
  overflowX: string;
  overflowY: string;
  /** `transform`, `filter` or `contain` makes this node the containing block of a
   *  `position: fixed` descendant (and of an `absolute` one). */
  containsFixed: boolean;
  /** The node is laid out by the page at all: `false` inside a `<select>` (its
   *  options are the control's, not the page's) or inside a closed `<details>`
   *  past its summary — both report an empty box while showing nothing. */
  rendered: boolean;
}

export interface LayoutView<N> {
  parent(node: N): N | null;
  layout(node: N): NodeLayout;
}

export interface Room {
  width: number;
  height: number;
}

const CLIPS = new Set(['hidden', 'clip']);

/** Is the node shown at all? `display: none` anywhere on its path, `display:
 *  contents` on the node itself (it has no box to measure), `visibility` other than
 *  `visible` (inherited, so the node's own value says it), or a node the page does
 *  not lay out → not walked. */
export function isWalked<N>(node: N, view: LayoutView<N>): boolean {
  const own = view.layout(node);
  if (own.display === 'contents' || own.visibility !== 'visible') return false;
  for (let n: N | null = node; n !== null; n = view.parent(n)) {
    const l = view.layout(n);
    if (l.display === 'none' || !l.rendered) return false;
  }
  return true;
}

/** The room the layout gave `node`: its box, capped in each dimension by every
 *  ancestor whose overflow clips it in that dimension. An ancestor clips only a
 *  descendant it contains: a `position: absolute` element escapes every clipping
 *  ancestor below its containing block (the nearest positioned ancestor), a
 *  `position: fixed` one every ancestor below one that contains fixed boxes. */
export function roomOf<N>(node: N, view: LayoutView<N>): Room {
  const own = view.layout(node);
  let width = own.width;
  let height = own.height;
  // What the chain so far escapes: nothing, ancestors up to a positioned one, or
  // ancestors up to one that contains fixed boxes.
  let escape: 'none' | 'absolute' | 'fixed' = escapeOf(own.position);
  for (let n = view.parent(node); n !== null; n = view.parent(n)) {
    const l = view.layout(n);
    const isContainingBlock =
      escape === 'none' ||
      (escape === 'absolute' && (l.position !== 'static' || l.containsFixed)) ||
      (escape === 'fixed' && l.containsFixed);
    if (!isContainingBlock) continue;
    // `overflow` does not apply to a box-less or inline (non-replaced) box.
    if (l.display !== 'contents' && l.display !== 'inline') {
      if (CLIPS.has(l.overflowX)) width = Math.min(width, l.width);
      if (CLIPS.has(l.overflowY)) height = Math.min(height, l.height);
    }
    // This ancestor's own positioning decides what the chain escapes above it.
    escape = escapeOf(l.position);
  }
  return { width: Math.max(0, width), height: Math.max(0, height) };
}

function escapeOf(position: string): 'none' | 'absolute' | 'fixed' {
  if (position === 'absolute') return 'absolute';
  if (position === 'fixed') return 'fixed';
  return 'none';
}

/** The pane: the room of `node`'s nearest ancestor that has a box (`null` when
 *  none has), and `depth`, the box-less (`display: contents`) ancestors between. */
export function paneOf<N>(node: N, view: LayoutView<N>): { pane: Room | null; depth: number } {
  let depth = 0;
  for (let n = view.parent(node); n !== null; n = view.parent(n)) {
    if (view.layout(n).display === 'contents') {
      depth += 1;
      continue;
    }
    return { pane: roomOf(n, view), depth };
  }
  return { pane: null, depth };
}
