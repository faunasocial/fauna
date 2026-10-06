// The region content plane's web shell (`region-blocking.md` § The content
// plane) — the twin of linux `region.rs` and tui `region.rs`. Everything that
// decides lives in shared Rust behind `WasmRegionPlane`
// (`libs/fauna-wasm/src/region.rs` over `libs/fauna-client-region`); what stays
// here is what the design lets diverge:
//
//   * the leaf — `navigator.language`, the browser's user-set language, handed
//     over raw (its region subtag is parsed in shared Rust; a tag with none
//     declares nothing);
//   * where the device record's bytes live — IndexedDB, install-scoped (a region
//     is a fact about the device, not the account; an envelope may reach 4 MiB,
//     past what `localStorage` holds);
//   * when to refresh — at connect and on the shared cadence tick;
//   * the paint (`RegionPlaceholder.svelte`, the settings section).
//
// A `.svelte.ts` module so `revision` is `$state`: every surface that composed a
// verdict reads it through `regionRender`, so a refresh that changed the record
// repaints the feed, the post detail and the conversation bubbles.

import {
  ensureWasm,
  openRegionPlane,
  type ContentLabelEntry,
  type ContentPolicyValue,
  type ContentRender,
  type RegionItem,
  type RegionPlaneHandle,
  type RegionView,
} from '$lib/wasm';
import { regionRefresh } from '$lib/rpc';
import { registerActorScopedReset } from '$lib/actorScope';

/** The language the authority's reason is picked in — the app's UI language
 *  (linux/tui `UI_LANG`). */
const UI_LANG = 'en';

const DB_NAME = 'fauna-region';
const STORE = 'record';
const KEY = 'device';

let plane: RegionPlaneHandle | null = null;
let opening: Promise<void> | null = null;
let refreshing = false;
/** Bumped whenever what the plane answers may have changed (opened, refreshed). */
let revision = $state(0);

function idb(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const req = indexedDB.open(DB_NAME, 1);
    req.onupgradeneeded = () => req.result.createObjectStore(STORE);
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

async function readRecord(): Promise<Uint8Array | null> {
  try {
    const db = await idb();
    try {
      return await new Promise((resolve, reject) => {
        const req = db.transaction(STORE, 'readonly').objectStore(STORE).get(KEY);
        req.onsuccess = () => resolve(req.result instanceof Uint8Array ? req.result : null);
        req.onerror = () => reject(req.error);
      });
    } finally {
      db.close();
    }
  } catch (e) {
    // No record reads as none: the plane starts empty and the first fetch fills
    // it (§ Fail posture — nothing held, nothing applied, never a relaxation of
    // something held, since nothing was).
    console.warn('region: device record unreadable:', e);
    return null;
  }
}

async function writeRecord(bytes: Uint8Array): Promise<void> {
  const db = await idb();
  try {
    await new Promise<void>((resolve, reject) => {
      const tx = db.transaction(STORE, 'readwrite');
      tx.objectStore(STORE).put(bytes, KEY);
      tx.oncomplete = () => resolve();
      tx.onerror = () => reject(tx.error);
    });
  } finally {
    db.close();
  }
}

/** Open the device's plane once per page, the persisted record loaded AHEAD of
 *  the first fetch (§ Fail posture). Idempotent. */
export function openRegion(): Promise<void> {
  if (!opening) {
    opening = (async () => {
      await ensureWasm();
      const record = await readRecord();
      const tag = typeof navigator !== 'undefined' ? navigator.language || null : null;
      plane = openRegionPlane(tag, record);
      revision += 1;
    })();
  }
  return opening;
}

/** Refresh through the session's nest when due (`force` at connect/login).
 *  A failed ask writes nothing; a changed record is persisted, then repainted. */
export async function refreshRegion(secretHex: string, force: boolean): Promise<void> {
  await openRegion();
  const p = plane;
  if (!p || refreshing) return;
  if (!force && !p.refreshDue()) return;
  refreshing = true;
  try {
    const bytes = await regionRefresh(secretHex, p);
    if (bytes) await writeRecord(bytes);
  } catch (e) {
    console.warn('region: refresh failed:', e);
  } finally {
    refreshing = false;
    revision += 1;
  }
}

/** The render decision for one item with the region composed in, or `null`
 *  before the plane is open (the caller then composes without the region). */
export function regionRender(
  labels: ContentLabelEntry[],
  contentPolicy: ContentPolicyValue | null,
  ownSpamPermille: number | undefined,
  ownPhishingPermille: number | undefined,
  item: RegionItem,
): ContentRender | null {
  void revision;
  if (!plane) return null;
  return plane.render(
    labels,
    contentPolicy,
    ownSpamPermille,
    ownPhishingPermille,
    item.contentIdHex ?? null,
    item.authorHex ?? null,
    item.text,
    item.hashtags,
    item.hasMedia,
    UI_LANG,
  );
}

/** What the Settings region section paints, or `null` before the plane opens. */
export function regionView(): RegionView | null {
  void revision;
  return plane ? plane.view() : null;
}

// ── Convention 17 (`region-block-never-silent`) ─────────────────────────────
//
// `blocked` is counted at the verdict by each mounted surface's own walk over
// the items it renders; `placeholders` is counted off the page as painted. Two
// sides of the render, so an arm that drops the placeholder (or the whole item)
// shows up as `placeholders < blocked` — tui's `block_render_json`.

const blockCounters = new Map<string, () => number>();

/** Register a surface's verdict-side walk while it is mounted; returns the
 *  unregister. */
export function registerRegionBlockCounter(surface: string, count: () => number): () => void {
  blockCounters.set(surface, count);
  return () => {
    if (blockCounters.get(surface) === count) blockCounters.delete(surface);
  };
}

/** The `region_block_render` state field the e2e agent publishes. */
export function regionBlockRenderForTest(): { blocked: number; placeholders: number } {
  let blocked = 0;
  for (const count of blockCounters.values()) blocked += count();
  const placeholders = document.querySelectorAll(
    '[data-testid="region-blocked-notice"][data-verdict="block"]',
  ).length;
  return { blocked, placeholders };
}

/** Forget the refresh clock on an identity change, so the next login asks at
 *  once. The plane itself is the device's and stays. */
function resetRegionSession(): void {
  plane?.clearSession();
}

registerActorScopedReset(resetRegionSession);
