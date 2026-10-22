// The member's side of the nest rules on the web SPA (`nest-rules.md` § The nest
// content policy → *Carriage*, § Acceptance at joining, and a change of rules)
// — the twin of tui `nest_rules.rs`. Everything that decides lives in shared Rust
// behind `WasmNestRulesBook` (`libs/fauna-wasm/src/nest_rules.rs` over
// `fauna_client_policy::NestRulesBook`): verifying the nest's signature, keeping
// the last-accepted document per nest, saying whether a read was a change, the
// rule sets the engine folds and the summary the settings surface paints. What
// stays here is what the design lets diverge:
//
//   * where the book's records live — IndexedDB, install-scoped like the region
//     record (a nest's signed document is public, keyed by nest identity, never
//     a secret), so a change made while the page was closed is still found by its
//     newer sequence;
//   * when to read — at every connect and on every `StaleSurfaces::nest_info`
//     push (the nest's `fauna.nest.info_changed` on an accepted rules write);
//   * the paint (`RegionPlaceholder.svelte`'s nest branch, the changed notice,
//     the settings section).
//
// A `.svelte.ts` module so `revision` and the notice are `$state`: every surface
// that composed a verdict reads the book through `regionRender`, so a read that
// changed the rules repaints the feed, the post detail and the conversation
// bubbles.

import { ensureWasm, openNestRulesBook, type NestRulesBookHandle, type NestRulesSummaryValue } from '#lib/wasm';
import { nestRulesRefresh } from '#lib/rpc';
import { registerActorScopedReset } from '#lib/actorScope';
import { t } from '#lib/i18n/strings';

/** The language the admin's reason is picked in — the app's UI language (tui
 *  `UI_LANG`). */
export const NEST_RULES_LANG = 'en';

const DB_NAME = 'fauna-nest-rules';
const STORE = 'book';
const KEY = 'records';

let book: NestRulesBookHandle | null = null;
let opening: Promise<void> | null = null;
let reading = false;
/** The session nest's identity, once its self-description was read — which nest
 *  every item on this page is read through. */
let sessionNestId: string | null = null;
/** Bumped whenever what the book answers may have changed. */
let revision = $state(0);
/** The one-line changed notice naming the nest, until the member opens the rules
 *  surface; `null` when there is none. */
let changedNoticeDomain = $state<string | null>(null);

function idb(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const req = indexedDB.open(DB_NAME, 1);
    req.onupgradeneeded = () => req.result.createObjectStore(STORE);
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

async function readRecords(): Promise<unknown | null> {
  try {
    const db = await idb();
    try {
      return await new Promise((resolve, reject) => {
        const req = db.transaction(STORE, 'readonly').objectStore(STORE).get(KEY);
        req.onsuccess = () => resolve(req.result ?? null);
        req.onerror = () => reject(req.error);
      });
    } finally {
      db.close();
    }
  } catch (e) {
    // No record reads as none: the book starts empty and the first read fills
    // it. The nest's rules then bind from that read (nothing held, nothing to
    // relax).
    console.warn('nest rules: stored book unreadable:', e);
    return null;
  }
}

async function writeRecords(records: unknown): Promise<void> {
  const db = await idb();
  try {
    await new Promise<void>((resolve, reject) => {
      const tx = db.transaction(STORE, 'readwrite');
      tx.objectStore(STORE).put(records, KEY);
      tx.oncomplete = () => resolve();
      tx.onerror = () => reject(tx.error);
    });
  } finally {
    db.close();
  }
}

/** Open the page's book once, the persisted records loaded AHEAD of the first
 *  read (each is verified again). Idempotent. */
export function openNestRules(): Promise<void> {
  if (!opening) {
    opening = (async () => {
      await ensureWasm();
      let records = await readRecords();
      try {
        book = openNestRulesBook(records);
      } catch (e) {
        // A stored book this build cannot restore starts the book empty.
        console.warn('nest rules: stored book not restorable:', e);
        records = null;
        book = openNestRulesBook(null);
      }
      revision += 1;
    })();
  }
  return opening;
}

/** Read the session nest's self-description past the memo and fold it. Called at
 *  every connect and on every `nest_info` push. A failed read leaves the
 *  last-known rules in force; a changed one persists the book and sets the
 *  notice. */
export async function refreshNestRules(secretHex: string): Promise<void> {
  await openNestRules();
  const b = book;
  if (!b || reading) return;
  reading = true;
  try {
    const { nestId, read } = await nestRulesRefresh(secretHex, b);
    sessionNestId = nestId;
    if (read.kind === 'changed') {
      changedNoticeDomain = read.domain;
      await writeRecords(b.stored()).catch((e) => console.warn('nest rules: persist failed:', e));
    } else if (read.kind === 'first') {
      await writeRecords(b.stored()).catch((e) => console.warn('nest rules: persist failed:', e));
    } else if (read.kind === 'refused') {
      console.warn('nest rules: self-description refused, last-known rules stay:', read.reason);
    }
  } catch (e) {
    console.warn('nest rules: fauna.nest.info failed:', e);
  } finally {
    reading = false;
    revision += 1;
  }
}

/** The book and the session nest it binds, or `null` before both exist — what
 *  `regionRender` passes to the composed call. */
export function nestRulesSource(): { book: NestRulesBookHandle; nestId: string } | null {
  void revision;
  return book && sessionNestId ? { book, nestId: sessionNestId } : null;
}

/** The changed notice's nest domain, or `null`. */
export function nestRulesChangedNotice(): string | null {
  void revision;
  return changedNoticeDomain;
}

/** The changed notice as painted — the frame naming the nest — or `null`; the
 *  e2e agent's `nest_rules_changed_notice` state field. */
export function nestRulesChangedNoticeText(): string | null {
  void revision;
  return changedNoticeDomain === null ? null : t.nest_rules.changed_notice({ nest: changedNoticeDomain });
}

/** The member opened the rules surface — the notice has done its job. */
export function dismissNestRulesNotice(): void {
  changedNoticeDomain = null;
}

/** The rules in force for the session nest, as the settings surface paints
 *  them, or `null` when the nest published none (or nothing is read yet). */
export function nestRulesSummary(): NestRulesSummaryValue | null {
  void revision;
  if (!book || !sessionNestId) return null;
  return book.summary(sessionNestId, NEST_RULES_LANG);
}

// ── Convention 17 (`nest-block-never-silent`) ───────────────────────────────
//
// `blocked` is counted at the verdict by each mounted surface's own walk over
// the items it renders; `placeholders` is counted off the page as painted. Two
// sides of the render, so an arm that drops the placeholder (or the whole item)
// shows up as `placeholders < blocked` — the region field's twin.

const blockCounters = new Map<string, () => number>();

/** Register a surface's verdict-side walk while it is mounted; returns the
 *  unregister. */
export function registerNestBlockCounter(surface: string, count: () => number): () => void {
  blockCounters.set(surface, count);
  return () => {
    if (blockCounters.get(surface) === count) blockCounters.delete(surface);
  };
}

/** The `nest_block_render` state field the e2e agent publishes. */
export function nestBlockRenderForTest(): { blocked: number; placeholders: number } {
  let blocked = 0;
  for (const count of blockCounters.values()) blocked += count();
  const placeholders = document.querySelectorAll(
    '[data-testid="nest-blocked-notice"][data-verdict="block"]',
  ).length;
  return { blocked, placeholders };
}

/** An identity change forgets which nest this page reads through; the book is the
 *  device's and stays. The next connect reads the new session nest. */
function resetNestRulesSession(): void {
  sessionNestId = null;
  changedNoticeDomain = null;
  revision += 1;
}

registerActorScopedReset(resetNestRulesSession);
