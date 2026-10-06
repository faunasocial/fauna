// The web leg of `fauna_e2e_agent::PAINTED_ERRORS_KEY` — every error surface a
// painted frame showed, counted by the natives' own `PaintedErrorTally`
// (`libs/fauna-wasm/src/e2e_observables.rs`), so "a passing connection gap
// raises no error anywhere" is the counter not moving rather than a read after
// the gap that would miss an error raised and cleared inside it.
//
// **One observation per painted frame that changed.** A `MutationObserver` over
// the document marks the page dirty; the next `requestAnimationFrame` — which
// runs just before the browser paints that frame — reads the frame's visible
// error surfaces once and hands them to the tally. A DOM change undone before
// the frame paints is never seen, exactly as it is never painted; one that
// paints always is.
//
// Test bundles only: the sole importer is `$lib/e2e-automation` (convention 15).

import { paintedErrorsObserveForTest } from '$lib/wasm';

/** An *error surface* in the contract's sense: the page's `error-message`, or a
 *  per-action `…-error` line — never a `…-error-log`. The tally re-applies the
 *  same predicate (and drops empty text), so this selector only narrows. */
const ERROR_SURFACES = '[data-testid="error-message"], [data-testid$="-error"]';

let installed = false;
let frameQueued = false;

/** Painted means laid out with a box — the same visibility rule
 *  `web-bridge/server.py`'s `/registry` frame applies. */
function isPainted(el: Element): boolean {
  if (!el.isConnected) return false;
  const style = getComputedStyle(el);
  if (style.display === 'none' || style.visibility === 'hidden' || style.visibility === 'collapse') {
    return false;
  }
  const rect = el.getBoundingClientRect();
  return rect.width > 0 && rect.height > 0;
}

function observeFrame(): void {
  frameQueued = false;
  const ids: string[] = [];
  const texts: string[] = [];
  for (const el of document.querySelectorAll(ERROR_SURFACES)) {
    if (!isPainted(el)) continue;
    ids.push(el.getAttribute('data-testid') ?? '');
    texts.push(((el as HTMLElement).innerText ?? el.textContent ?? '').trim());
  }
  paintedErrorsObserveForTest(ids, texts);
}

function scheduleFrame(): void {
  if (frameQueued) return;
  frameQueued = true;
  requestAnimationFrame(observeFrame);
}

/** Start observing. Idempotent. */
export function installPaintedErrorObserver(): void {
  if (installed || typeof document === 'undefined') return;
  installed = true;
  new MutationObserver(scheduleFrame).observe(document.documentElement, {
    subtree: true,
    childList: true,
    characterData: true,
    // Visibility flips ride class/style/hidden changes, not only node churn.
    attributes: true,
    attributeFilter: ['class', 'style', 'hidden', 'data-testid'],
  });
  scheduleFrame();
}
