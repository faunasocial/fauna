// Search module (web) — the single SPA home for the shared (wasm)
// `SearchManager`. The browser twin of tui `src/search.rs` (which renders the
// Search page entirely from `SearchManager::snapshot()` — the manager owns
// the query/filter/paging/merge decisions, and the page is a paint shell,
// `docs/goal/ui/search.md` § State & data shape, ratified 2026-08-02). Owns:
//
//   1. The process-wide manager singleton (`getSearchManager`), built once the
//      identity is known. Unlike the Feed manager it needs no actor secret —
//      searching signs nothing.
//   2. The reactive store the page reads off: `searchSnapshot` (the raw
//      `manager.snapshot()` — query, type filter, results, in-flight,
//      no-results, has-more, error).
//
// No foreign `SearchSnapshotObserver` callback crosses into JS: the browser
// owns the loop, so the page `await`s each async manager method then calls
// `refreshSearch()` to re-read the snapshot — the same snapshot-after-call
// reactivity contract `$lib/feed.ts` uses.

import { get, writable, type Writable } from 'svelte/store';
import { identity } from './store';
import { guardSingletonBuild } from './singleton-build';
import { registerActorScopedReset, sameActorSince } from './actorScope';
import { ensureWasm, logMessage } from './wasm';
import { searchManager } from './rpc';
import type { WasmSearchManager } from '../../static/fauna_wasm.js';

/** The raw `manager.snapshot()` — the shared `SearchSnapshot` serde JSON the
 *  Search page renders its query bar, cancel button, and results/no-results
 *  pair entirely from (`search.md` § State & data shape). `null` until the
 *  manager is built. `any` because the shape is the shared Rust snapshot. */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export const searchSnapshot: Writable<any> = writable(null);

let manager: WasmSearchManager | null = null;
let managerPromise: Promise<WasmSearchManager> | null = null;

/** The single shared `SearchManager` for the SPA. Built once the identity is
 *  known (searching needs no secret — the connection alone is enough). The
 *  Search page drives this one instance; the snapshot starts at the pre-search
 *  state. Rejects until an identity exists. */
export function getSearchManager(): Promise<WasmSearchManager> {
  if (managerPromise) return managerPromise;
  const id = get(identity);
  if (!id?.secretHex) {
    return Promise.reject(new Error('search manager: no identity yet'));
  }
  // The identity seam ahead of the write below (`actorScope.ts`), the same one
  // `getFeedManager` / `getConversationsManager` / `getEventDrafts` have carried
  // since the three-rail seam landed — search was the FOURTH rail and was missed. A switch landing mid-build is not stopped by
  // `resetSearchManager`, which only nulls `manager`/`managerPromise`: this build
  // resolves whatever the switch did and would assign the DEPARTING actor's
  // manager. Searching signs nothing, so this leaks no key — but it renders the
  // outgoing actor's results to the incoming one, which is the class 1/4 breach
  // `account-scoping.md` § The scoping taxonomy's in-memory corollary names, and
  // the drop exists to prevent.
  const stillThisActor = sameActorSince();
  const build = async (stillWanted: () => boolean): Promise<WasmSearchManager> => {
    await ensureWasm();
    const built = await searchManager(id.secretHex);
    if (!stillThisActor()) {
      throw new Error('search manager: actor changed while the manager was building');
    }
    // The settle deadline's seam beside the identity one — an abandoned build
    // resolving after its replacement must not install itself over it
    // (`singleton-build.ts`).
    if (!stillWanted()) {
      throw new Error('search manager: build abandoned by its settle deadline');
    }
    manager = built;
    return built;
  };
  // A failed build must not be MEMOIZED, and a build that never SETTLES must
  // not be memoized either — the two halves of "a singleton build must reach a
  // terminal state", both discharged by the shared guard. A promise memo caches
  // a rejection as durably as a value, so without the clear one transient
  // failure makes search permanently unbuildable for the page's life; and a
  // wasm task that dies mid-poll never rejects at all, so no `.catch` and no
  // internal deadline can see it. The actor-scoped drop saves neither (it runs
  // on an identity CHANGE, not on the same actor re-entering the route).
  // `=== guarded` is what keeps the clear from throwing away a newer build the
  // drop has since installed. Pinned by
  // `singleton-build-memo-contract.test.ts`; mechanism in `singleton-build.ts`.
  const guarded = guardSingletonBuild('search manager', build, () => {
    if (managerPromise !== guarded) return;
    managerPromise = null;
    // Retract the slot with the memo, as every builder does (`feed.ts` says
    // why). This build installs last, so here it can only be empty already —
    // kept anyway so the shape stays one shape across the census.
    manager = null;
  });
  managerPromise = guarded;
  return guarded;
}

/** Tear down the singleton (`identity.logout()` calls this) — the same
 *  actor-scoping reset every other manager singleton registers, so a soft-nav
 *  sign-out → sign-in-as-a-different-identity never renders the PREVIOUS
 *  actor's search results to the NEW actor. The next `getSearchManager()` call
 *  builds fresh. */
export function resetSearchManager(): void {
  manager = null;
  managerPromise = null;
  searchSnapshot.set(null);
}

registerActorScopedReset(resetSearchManager);

/** Re-read the manager snapshot into the reactive store. Called after every
 *  async manager method resolves (run-query / load-more / cancel) — the
 *  snapshot-after-call reactivity contract (no observer→JS callback, same as
 *  the wasm feed/conversations/admin machines). A no-op before the manager is
 *  built. */
export function refreshSearch(): void {
  if (!manager) return;
  try {
    searchSnapshot.set(manager.snapshot());
  } catch (e) {
    console.warn('search snapshot failed:', e);
    logMessage('warn', 'fauna_web::search', `search snapshot failed: ${e}`);
  }
}

// ── Result-row navigation (`search.md` § User actions —
//    "search-result-item[i] | Open destination", ratified 2026-08-02) ────────
//
// Activating a `search-result-item` routes by the row's typed `SearchNav`
// target (tui landed this 2026-08-10 — `apps/fauna-tui/src/search.rs`'s
// `open_result`). web's destination pages (`feed`, `conversations`) are each a
// SEPARATE `+page.svelte` component with its own LOCAL `$state` — unlike tui's
// single `App` struct, a SvelteKit client-side navigation unmounts the Search
// page and mounts a fresh destination component, which starts with no memory
// of what the user just clicked. This store is the SPA's answer: the Search
// page stashes the target here immediately before `goto()`-ing away, and the
// destination page's own `onMount` calls `consumePendingSearchNav()` once
// (after its own manager/snapshot is ready) to read-and-clear it — a plain
// one-shot handoff, no query-string plumbing, no back-button side effects.
//
// Every `SearchNav` target is representable here (2026-08-14 — `resolvePost`,
// `carddavLocateCardByUidHash`, `locateFileJson` all landed as wasm exports,
// closing the gap this comment used to describe). `Contact { uid_hash }` and
// `File { folder_id, path_hash }` (ui/search.md § State & data shape) are id-
// space RESOLVES, not casts: the row's identity and the destination page's key
// are deliberately different spellings ("A naive cast between the two contact
// id spaces compiles, runs, opens nothing, and raises nothing — silent
// failure, not a crash" — search.md's own warning), so `contact`/`file` here
// carry the row's raw id and the destination page (Contacts / Media) does the
// shared-Rust lookup itself in its own `onMount`, exactly as `post` does not
// carry a resolved post — `openPostDetail` resolves it.
export type PendingSearchNav =
  | { kind: 'post'; postId: string }
  | { kind: 'thread'; threadId: string }
  | { kind: 'compose' }
  | { kind: 'contact'; uidHash: string }
  | { kind: 'file'; folderId: number; pathHash: string };

const pendingSearchNav: Writable<PendingSearchNav | null> = writable(null);

/** Called by the Search page right before navigating away, once per
 *  activation. */
export function setPendingSearchNav(target: PendingSearchNav): void {
  pendingSearchNav.set(target);
}

/** Called once by a destination page's `onMount` (after its own manager is
 *  ready) to read AND CLEAR the pending target — a plain page load (no click
 *  routed here) sees `null` and proceeds exactly as it does today. */
export function consumePendingSearchNav(): PendingSearchNav | null {
  const v = get(pendingSearchNav);
  if (v) pendingSearchNav.set(null);
  return v;
}
