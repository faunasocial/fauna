// Feed module (web) — the single SPA home for the shared (wasm) `FeedManager`.
// The browser twin of linux `src/views/feed/` (which renders the Feed page
// entirely from `FeedManager::snapshot()` via a `SnapshotObserver`). Owns:
//
//   1. The process-wide manager singleton (`getFeedManager`), built once the
//      identity is known — its 32-byte secret seeds post building + signing on
//      `submitPost`. One instance backs the selector, post list, composer, and
//      bridge form — one page, one shared post-list/compose/feed-rule state
//      (`docs/goal/ui/feed.md` § State & data shape, ratified 2026-06-14).
//   2. The reactive store the page + e2e read off: `feedSnapshot` (the raw
//      `manager.snapshot()` — feeds, bridge feeds, selection, ordered post
//      list, search term, status, compose, bridge form, page error).
//
// Observer-driven rendering (`feed.md` § Architectural rules #1): the built
// manager is subscribed (`subscribe`) and every notification schedules one
// `refreshFeed()` re-read, so a publish made MID-call — a switch clearing its
// list before the fetch, a resolve folding an embed in — reaches the page when
// it happens, not when the call resolves. The page still calls `refreshFeed()`
// after the calls it awaits; that is a harmless extra re-read, not the
// contract.

import { get, writable, type Writable } from 'svelte/store';
import { identity } from './store';
import { guardSingletonBuild } from './singleton-build';
import { registerActorScopedReset, sameActorSince } from './actorScope';
import { autosaveDebounceMs, ensureWasm, logMessage } from './wasm';
import { feedManager } from './rpc';
import type { WasmFeedManager } from '../../static/fauna_wasm.js';

/** The raw `manager.snapshot()` — the shared `FeedSnapshot` serde JSON the Feed
 *  page renders its selector, post list, compose bar, bridge form, and page
 *  error entirely from (`feed.md` § State & data shape). `null` until the
 *  manager is built + first refreshed. `any` because the shape is the shared
 *  Rust snapshot. */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export const feedSnapshot: Writable<any> = writable(null);

let manager: WasmFeedManager | null = null;
let managerPromise: Promise<WasmFeedManager> | null = null;

/** The built feed manager, or `null` — never builds. The twin of
 *  `conversationsManagerIfReady`, for a caller that may act only on a manager
 *  something else already built: the conversations tick installing the
 *  room-post seam (`conversations.ts`'s `syncFeedRoomPosts`) must not construct
 *  a Feed manager for a session that never opened the feed. */
export function feedManagerIfReady(): WasmFeedManager | null {
  return manager;
}

/** The single shared `FeedManager` for the SPA. Built once the identity is
 *  known (its 32-byte secret seeds the `submitPost` post signer). The Feed page
 *  drives this one instance; the snapshot starts empty + `Loading`. Rejects
 *  until an identity exists. */
export function getFeedManager(): Promise<WasmFeedManager> {
  if (managerPromise) return managerPromise;
  const id = get(identity);
  if (!id?.secretHex) {
    return Promise.reject(new Error('feed manager: no identity yet'));
  }
  // The identity seam ahead of the write below (`actorScope.ts`): a switch
  // landing mid-build is not stopped by `resetFeedManager`, which only nulls
  // `manager`/`managerPromise` — this build resolves whatever the switch did and
  // would assign the DEPARTING actor's manager, restoring exactly the
  // wrong-actor render (its post list, including decrypted gated-post bodies)
  // that reset exists to prevent.
  const stillThisActor = sameActorSince();
  const build = async (stillWanted: () => boolean): Promise<WasmFeedManager> => {
    await ensureWasm();
    const built = await feedManager(id.secretHex);
    if (!stillThisActor()) {
      throw new Error('feed manager: actor changed while the manager was building');
    }
    // The settle deadline's seam, one line from the identity one: a build the
    // guard gave up on can still be alive, and resolving here after a later
    // mount has installed its replacement it would put itself in `manager`
    // while `managerPromise` vends the other one (`singleton-build.ts`).
    if (!stillWanted()) {
      throw new Error('feed manager: build abandoned by its settle deadline');
    }
    manager = built;
    // Observer-driven rendering (the header): the notify runs inside the
    // manager's own mutation, so it only queues the re-read.
    built.subscribe({ onChanged: scheduleFeedRefresh });
    // Draft-persistence v2, posts rail (feed.md § Persistence): restore the
    // owner's persisted feed-composer draft once, before any save can run — so
    // an in-progress post survives a restart and appears on the user's other
    // devices. Every caller of this singleton (the Feed page, quote/reply from
    // conversations, PersonalizationSection) awaits the SAME promise, so this
    // runs exactly once per manager build regardless of who triggers it —
    // matching `getConversationsManager`'s restoreDrafts shape one rail over.
    // A transient/seal failure is logged + swallowed (the page must still
    // open); the wasm side's shared `DraftsSync` keeps its save gate closed
    // until a restore *succeeds*, so a later save can't clobber an unread
    // (incl. undecryptable) blob — the next launch retries the load.
    try {
      await built.restoreDrafts();
    } catch (e) {
      logMessage('warn', 'fauna_web::feed', `restore drafts failed: ${e}`);
    }
    return built;
  };
  // ⚠ A PROMISE MEMO CACHES A REJECTION AS DURABLY AS A VALUE. Without this
  // clear, one failed build — `ensureWasm()` losing a race, the WS-RPC connect
  // failing, the nest answering 500 while the box is loaded — is handed to
  // every later caller for the rest of the page's life. The manager is then not
  // "still booting"; it is permanently unbuildable, and nothing retries.
  //
  // The drop (`resetFeedManager`) cannot save it: that runs on an identity
  // CHANGE, and the commonest re-entry is the SAME actor mounting the route
  // again — where `store.ts`'s subscription early-returns on the unchanged
  // `secretHex` and no handler re-fires at all. Measured:
  // every `test_feed.py` journey logs in as one shared actor, so a single
  // transient failure did not cost ONE journey — it failed every feed journey
  // after it in the module, identically, `post-submit-button` disabled for the
  // full 90 s ceiling with an EMPTY `error-message`, until the next
  // module-boundary relaunch. 3–4 of 22 red per run, standalone and docker alike.
  //
  // `=== guarded` is what makes this safe on the actor-changed throw above: by
  // then the drop may already have installed the INCOMING actor's build, and
  // clearing unconditionally would throw that away (the hazard
  // `getConversationsManager` names at its own supersede throw). Same remedy it
  // applies at its engine-role refusal, generalized to every way a build can
  // fail; `ensureWasm` has kept it since `wasm-loader-guard.test.ts`.
  //
  // ⚠ AND THE OTHER HALF: a build that never SETTLES is memoized exactly as
  // durably, and the clear above cannot see it — `.catch` never fires for a
  // promise with no terminal state. That is not hypothetical on this path: a
  // wasm task whose poll throws dies mid-poll without settling its JS promise,
  // taking `ensureConnected`'s 15 s throw and `restoreDrafts`'s 5 s kind
  // deadline down with it, so every "this build is bounded" proof lapses at
  // once. `guardSingletonBuild` adds the external 45 s settle deadline that
  // survives the task's death and routes into this same clear — and hands the
  // build the `stillWanted` it checks above, because a deadline can abandon a
  // build it cannot stop.
  const guarded = guardSingletonBuild('feed manager', build, () => {
    if (managerPromise !== guarded) return;
    managerPromise = null;
    // Retract the slot with the memo. A build can be abandoned AFTER its
    // install — in a draft restore whose task then died — and while the memo
    // still held it nothing else could write `manager`, so the slot holds this
    // build's manager or nothing. Leaving it would keep every `manager.` reader
    // on a build the page was told had failed (`singleton-build.ts`).
    manager = null;
  });
  managerPromise = guarded;
  return guarded;
}

// ── Draft persistence (v2) — the compose-change SAVE trigger ─────────────────
//
// The feed twin of `conversations.ts`'s `scheduleDraftSave` one rail over: the
// Feed page calls this from its compose-mutator chokepoint (`syncCompose`); we
// coalesce a burst of keystrokes into one debounced `manager.saveDrafts()`
// (snapshot → seal → `fauna.drafts.put`, all in wasm — feed.md § Persistence).
// Fire-and-forget: a transient failure is logged, never surfaced on the page (a
// not-yet-synced draft is not a user-facing error). The restore-on-launch half
// lives in `getFeedManager` above.

// Under the Playwright e2e agent, debounce far shorter so a restart round-trip
// test (`test_feed_draft_persistence.py`) needn't wait the production window —
// mirrors `conversations.ts`'s identical E2E cadence.
const DRAFT_SAVE_DEBOUNCE_E2E_MS = 150;
let draftSaveTimer: ReturnType<typeof setTimeout> | null = null;

function draftSaveDebounceMs(): number {
  const hasAgent =
    __FAUNA_E2E_AUTOMATION__ &&
    typeof window !== 'undefined' &&
    (window as unknown as { __faunaTestAgent?: unknown }).__faunaTestAgent != null;
  return hasAgent ? DRAFT_SAVE_DEBOUNCE_E2E_MS : autosaveDebounceMs();
}

/** Debounced persist of the feed-composer draft after a compose change
 *  (feed.md § Persistence). Coalesces a burst of edits into one
 *  `fauna.drafts.put`; fire-and-forget (errors logged, never surfaced). */
export function scheduleDraftSave(): void {
  if (draftSaveTimer) clearTimeout(draftSaveTimer);
  draftSaveTimer = setTimeout(() => {
    draftSaveTimer = null;
    if (!manager) return;
    void manager.saveDrafts().catch((e) => {
      logMessage('debug', 'fauna_web::feed', `save drafts failed (transient): ${e}`);
    });
  }, draftSaveDebounceMs());
}

/** Force an immediate save, bypassing the debounce — the leave-door flush
 *  (`reserved-folders.md` § The leave-flush promise, row 481), called from the
 *  root layout's `visibilitychange`/`pagehide` handlers. Best-effort like every
 *  such handler on the web platform (the goal doc's own wording: a browser
 *  grants no reliable async work after either event) — mirrors
 *  `conversations.ts`'s twin one rail over. */
export function flushDraftsNow(): void {
  if (draftSaveTimer) {
    clearTimeout(draftSaveTimer);
    draftSaveTimer = null;
  }
  if (!manager) return;
  void manager.saveDrafts().catch((e) => {
    logMessage('debug', 'fauna_web::feed', `leave-flush drafts failed: ${e}`);
  });
}

/** Tear down the singleton (`identity.logout()` calls this). Without it a
 *  soft-nav sign-out → sign-in-as-a-different-identity within one page load
 *  (module state survives; sign-out never reloads) keeps rendering the
 *  PREVIOUS actor's `FeedManager` — its post list, including decrypted
 *  gated-post bodies — to the NEW actor, since `getFeedManager` returns the
 *  built `managerPromise` unconditionally regardless of the identity that
 *  built it. The next `getFeedManager()` call builds fresh against whichever
 *  identity is current at that time. */
export function resetFeedManager(): void {
  manager = null;
  managerPromise = null;
  feedSnapshot.set(null);
}

// The drop registers itself, so no switch handler has to know this module exists
// (`actorScope.ts` — account-scoping.md § The scoping taxonomy, in-memory
// corollary). This module is only ever loaded when something actually uses the
// feed, so a session that never opens it registers nothing and has nothing to drop.
registerActorScopedReset(resetFeedManager);

let feedRefreshQueued = false;

/** Queue ONE `refreshFeed()` for the next microtask — the manager's observer
 *  callback. Coalesces a burst of notifications into one re-read, and never
 *  re-enters the manager from inside the mutation that notified. */
function scheduleFeedRefresh(): void {
  if (feedRefreshQueued) return;
  feedRefreshQueued = true;
  queueMicrotask(() => {
    feedRefreshQueued = false;
    refreshFeed();
  });
}

/** Re-read the manager snapshot into the reactive store. Called after every
 *  async manager method resolves (select / search / load-more / submit / create
 *  / delete / subscribe / resolve-media / resolve-quote) — the
 *  snapshot-after-call reactivity contract (no observer→JS callback, same as
 *  the wasm conversations + admin machines). A no-op before the manager is
 *  built. */
export function refreshFeed(): void {
  if (!manager) return;
  try {
    feedSnapshot.set(manager.snapshot());
  } catch (e) {
    console.warn('feed snapshot failed:', e);
    logMessage('warn', 'fauna_web::feed', `feed snapshot failed: ${e}`);
  }
}

/** The manager's `{started, completed, committed_gen}` reload triple — the web
 *  read of `fauna_e2e_agent::FEED_RELOADS_KEY` (counting and JSON shape both
 *  live in shared Rust; the wasm face's `feedReloads()` is three atomic reads,
 *  no fetch). `committed_gen` is the generation of the newest COMMITTED reload
 *  and is the barrier's release condition — a commit *count* cannot be one,
 *  because a superseded reload never commits and so lags the count forever
 *  (`FEED_RELOADS_KEY`'s own contract). Zeros before the singleton is built —
 *  the legitimate "no reloads yet" answer the native apps publish pre-auth; the
 *  *no-leg* answer is the absent hook in `web-bridge/agent.js`, never a zero
 *  (convention 11). */
export function feedReloads(): FeedReloads {
  if (!manager) return { started: 0, completed: 0, committed_gen: 0 };
  return manager.feedReloads() as FeedReloads;
}

/** The shape `fauna_feed::feed_reloads_json` derives — see [[feedReloads]]. */
export type FeedReloads = { started: number; completed: number; committed_gen: number };
