// The web leg of `fauna_e2e_agent::ROOT_SURFACE_KEY` — what each painted frame
// showed at the tab's root, journaled by the natives' own
// `RootSurfaceJournal` (`libs/fauna-wasm/src/e2e_observables.rs`), so the
// in-place witness of `onboarding.md` § Wizard exit handling (the tab shows the
// launch progress view or the app from the wizard's exit on, never nothing)
// reads one entry per change rather than a poll that a short blank would pass.
//
// **One observation per painted frame that changed**, as `#lib/e2e-painted-errors`:
// a `MutationObserver` marks the page dirty and the next
// `requestAnimationFrame` — just before the browser paints — classifies it.
//
// **The classification.** A root that names itself — the onboarding page and
// the unlock gate carry `data-root-surface` in test builds — shows that name
// when it painted any text, and NOTHING (`null`) when it painted none (a step
// with no template arm). With no self-naming root on the page, the signed-in
// shell is up: `app` when the document painted any text, else `null`.
//
// Test bundles only: the sole importer is `#lib/loud-surface-e2e` (convention 15).

import { rootSurfaceObserveForTest } from '#lib/wasm';

export const ROOT_SURFACE_ATTR = 'data-root-surface';

let installed = false;
let frameQueued = false;

function paintedText(el: HTMLElement): boolean {
  return (el.innerText ?? '').trim() !== '';
}

function observeFrame(): void {
  frameQueued = false;
  let showing: string | null;
  const named = document.querySelector<HTMLElement>(`[${ROOT_SURFACE_ATTR}]`);
  if (named) {
    showing = paintedText(named) ? named.getAttribute(ROOT_SURFACE_ATTR) : null;
  } else {
    showing = document.body && paintedText(document.body) ? 'app' : null;
  }
  rootSurfaceObserveForTest(showing);
}

function scheduleFrame(): void {
  if (frameQueued) return;
  frameQueued = true;
  requestAnimationFrame(observeFrame);
}

/** Start observing. Idempotent. */
export function installRootSurfaceObserver(): void {
  if (installed || typeof document === 'undefined') return;
  installed = true;
  new MutationObserver(scheduleFrame).observe(document.documentElement, {
    subtree: true,
    childList: true,
    characterData: true,
    attributes: true,
    attributeFilter: ['class', 'style', 'hidden', ROOT_SURFACE_ATTR],
  });
  scheduleFrame();
}
