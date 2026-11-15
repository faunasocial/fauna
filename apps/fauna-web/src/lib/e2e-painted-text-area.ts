// The web leg of `fauna_e2e_agent::PAINTED_TEXT_AREA_KEY` — convention 17's
// `painted-text-has-area` field: every shown, `data-testid`-bearing element with
// text of its own, measured by the room the DOM layout gave it
// (`#lib/painted-text-room`, the measure's rules) and fed to the natives' own
// `fauna_e2e_contract::PaintedTextArea` (`libs/fauna-wasm/src/e2e_observables.rs`),
// which keeps the starved entries. tui and windows are the earlier legs.
//
// **One observation per painted frame**, beside the painted-error observer and on
// its trigger (`#lib/e2e-painted-errors`): a DOM change, or a resize, marks the page
// dirty and the next `requestAnimationFrame` measures once, just before the browser
// paints. The record is the last frame's, as on every app.
//
// Test bundles only: the sole importer is `#lib/loud-surface-e2e`, reached only from
// `#lib/e2e-automation` (convention 15).

import { type LayoutView, type NodeLayout, isWalked, paneOf, roomOf } from './painted-text-room';
import { paintedTextAreaObserveForTest } from './wasm';

/** Elements whose own text is never painted as page text. */
const NOT_TEXT = new Set(['SCRIPT', 'STYLE', 'TEMPLATE', 'NOSCRIPT']);

function domView(): LayoutView<Element> {
  const cache = new Map<Element, NodeLayout>();
  return {
    parent: (el) => el.parentElement,
    layout(el) {
      let l = cache.get(el);
      if (l) return l;
      const style = getComputedStyle(el);
      const rect = el.getBoundingClientRect();
      l = {
        width: rect.width,
        height: rect.height,
        display: style.display,
        visibility: style.visibility,
        position: style.position,
        overflowX: style.overflowX,
        overflowY: style.overflowY,
        containsFixed:
          style.transform !== 'none' ||
          style.filter !== 'none' ||
          /paint|layout|strict|content/.test(style.contain),
        rendered: isRenderedByPage(el),
      };
      cache.set(el, l);
      return l;
    },
  };
}

/** `false` for an option list's content (laid out by the control, not the page)
 *  and for a closed `<details>`' content past its summary. Checked per node; the
 *  walk applies it along the path. */
function isRenderedByPage(el: Element): boolean {
  const tag = el.tagName;
  if (tag === 'OPTION' || tag === 'OPTGROUP' || tag === 'DATALIST') return false;
  const parent = el.parentElement;
  if (parent?.tagName === 'SELECT') return false;
  if (parent?.tagName === 'DETAILS' && !(parent as HTMLDetailsElement).open && tag !== 'SUMMARY') {
    return false;
  }
  return true;
}

/** The element's own text: its text nodes, less those inside a descendant that
 *  carries a `data-testid` of its own (that one is measured itself) or inside a
 *  non-text element. Whitespace-collapsed. */
function ownText(el: Element): string {
  const parts: string[] = [];
  const walker = document.createTreeWalker(el, NodeFilter.SHOW_ELEMENT | NodeFilter.SHOW_TEXT, {
    acceptNode(node) {
      if (node.nodeType === Node.TEXT_NODE) return NodeFilter.FILTER_ACCEPT;
      const child = node as Element;
      if (child === el) return NodeFilter.FILTER_SKIP;
      if (NOT_TEXT.has(child.tagName) || child.hasAttribute('data-testid')) {
        return NodeFilter.FILTER_REJECT;
      }
      return NodeFilter.FILTER_SKIP;
    },
  });
  for (let n = walker.nextNode(); n !== null; n = walker.nextNode()) {
    parts.push(n.nodeValue ?? '');
  }
  return parts.join('').replace(/\s+/g, ' ').trim();
}

let installed = false;
let frameQueued = false;

function observeFrame(): void {
  frameQueued = false;
  const view = domView();
  const frame = {
    ids: [] as string[],
    texts: [] as string[],
    widths: [] as number[],
    heights: [] as number[],
    paneWidths: [] as number[],
    paneHeights: [] as number[],
    depths: [] as number[],
  };
  for (const el of document.querySelectorAll('[data-testid]')) {
    const id = el.getAttribute('data-testid') ?? '';
    if (!id || NOT_TEXT.has(el.tagName)) continue;
    const text = ownText(el);
    if (!text) continue;
    if (!isWalked(el, view)) continue;
    const room = roomOf(el, view);
    const { pane, depth } = paneOf(el, view);
    frame.ids.push(id);
    frame.texts.push(text);
    frame.widths.push(room.width);
    frame.heights.push(room.height);
    frame.paneWidths.push(pane ? pane.width : NaN);
    frame.paneHeights.push(pane ? pane.height : NaN);
    frame.depths.push(depth);
  }
  paintedTextAreaObserveForTest(frame);
}

function scheduleFrame(): void {
  if (frameQueued) return;
  frameQueued = true;
  requestAnimationFrame(observeFrame);
}

/** Start observing. Idempotent. */
export function installPaintedTextAreaObserver(): void {
  if (installed || typeof document === 'undefined') return;
  installed = true;
  new MutationObserver(scheduleFrame).observe(document.documentElement, {
    subtree: true,
    childList: true,
    characterData: true,
    attributes: true,
    attributeFilter: ['class', 'style', 'hidden', 'open', 'data-testid'],
  });
  window.addEventListener('resize', scheduleFrame);
  scheduleFrame();
}
