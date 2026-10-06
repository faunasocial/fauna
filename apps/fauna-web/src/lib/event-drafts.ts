/**
 * Web leg of draft-persistence v2 for the **events rail**
 * (`docs/goal/behavior/reserved-folders.md` § Drafts Sync;
 * `docs/goal/ui/events.md` § Persistence).
 *
 * The twin of `feed.ts`'s posts-rail glue and `conversations.ts`'s
 * conversations-rail glue, one rail over — and, like every other app's events
 * leg, deliberately a different *shape*: those two hang off a shared wasm
 * manager that already owns the compose state, so their save takes no
 * arguments. The Events page has no manager (the 2026-08-17 ruling in
 * `reserved-folders.md` § Drafts Sync: the three trigger shapes stay three), so
 * the page hands its five `event-form` inputs to `WasmEventDrafts.saveDrafts`
 * and gets them back from `restoreDrafts`.
 *
 * Everything that matters still stays in Rust — the canonical encoding, the seal
 * under the owner's `BackupKey`, the `fauna.drafts.{get,put}` calls, the launch
 * gate and the last-saved baseline (priority #2). This file only decides *when*.
 */
import { get } from 'svelte/store';

import { identity } from './store';
import { guardSingletonBuild } from './singleton-build';
import { registerActorScopedReset, sameActorSince } from './actorScope';
import { eventDrafts } from './rpc';
import { autosaveDebounceMs, ensureWasm, logMessage } from './wasm';
import type { WasmEventDrafts } from '../../static/fauna_wasm.js';

/** The five user-authored `event-form` inputs the rail rests — the JS mirror of
 *  `fauna_client_caldav::drafts::EventDrafts`. The calendar is deliberately NOT
 *  among them (it is per-device view state, resolved at submit), and the
 *  datetimes are raw as typed (`events.md` § Persistence). */
export interface EventDraft {
  summary: string;
  dtstart: string;
  dtend: string;
  description: string;
  location: string;
}

let face: WasmEventDrafts | null = null;
let facePromise: Promise<WasmEventDrafts> | null = null;

/** The draft this page-load knows about: seeded by the launch restore, then kept
 *  current by every {@link scheduleEventDraftSave}. It is module state, not
 *  component state, **on purpose** — the Events page unmounts when the user
 *  navigates away and its `$state` dies with it, so a draft parked in the
 *  component is gone the moment they visit Settings and come back, even though
 *  the nest still holds it. Keeping it here is also what makes web's rail the
 *  same shape as the native legs, where the rail object owns the live draft and
 *  the New Event opener reads it (`apps/fauna-linux/src/views/events/drafts.rs`,
 *  `apps/fauna-tui/src/events/drafts.rs`). */
let current: EventDraft | null = null;

/** Build (once) the events-rail face for the logged-in actor and run the launch
 *  restore, remembering what came back for {@link resumeEventDraft}.
 *
 *  Awaited by the Events page on mount, before any save can run — so an
 *  in-progress event survives a restart and appears on the user's other devices.
 *  A transient/seal failure is logged + swallowed (the page must still open);
 *  the wasm side's shared `DraftsSync` keeps its save gate closed until a
 *  restore *succeeds*, so a later save cannot clobber an unread (incl.
 *  undecryptable) blob — the next launch retries the load. */
export function loadEventDrafts(): Promise<WasmEventDrafts> {
  if (facePromise) return facePromise;
  const id = get(identity);
  if (!id?.secretHex) {
    return Promise.reject(new Error('event drafts: no identity yet'));
  }
  // The identity seam, ahead of every write below (`actorScope.ts` — the
  // generation is bumped by the drop). `resetEventDrafts` nulls `face`,
  // `facePromise` and `current`, but it cannot stop THIS build: both awaits
  // below resolve whatever the switch did, and the assignments that follow them
  // would hand the incoming actor the departing one's face (so B's saves seal
  // under A's `BackupKey`) and the departing one's restored draft (so B's first
  // keystroke persists A's text under B's).
  const stillThisActor = sameActorSince();
  const build = async (stillWanted: () => boolean): Promise<WasmEventDrafts> => {
    await ensureWasm();
    const built = await eventDrafts(id.secretHex);
    if (!stillThisActor()) {
      // The actor left while wasm was building this face. Rejecting rather than
      // returning it is deliberate: the caller awaited a face for an actor who
      // is gone, and handing it one that still seals under the old `BackupKey`
      // is the leak. The next `loadEventDrafts()` builds fresh for whoever is
      // current — `facePromise` was already nulled by the drop.
      throw new Error('event drafts: actor changed while the face was building');
    }
    // The settle deadline's seam beside the identity one: an abandoned build
    // resolving after its replacement must not install its face over the
    // replacement's (`singleton-build.ts`).
    if (!stillWanted()) {
      throw new Error('event drafts: build abandoned by its settle deadline');
    }
    face = built;
    try {
      const draft = (await built.restoreDrafts()) as EventDraft | undefined;
      // Both seams again, after the restore's own await: a switch or an
      // abandonment landing inside it must not seed `current` either — the
      // replacement build restores its own.
      if (draft && stillThisActor() && stillWanted()) current = draft;
    } catch (e) {
      logMessage('warn', 'fauna_web::events', `restore event drafts failed: ${e}`);
    }
    return built;
  };
  // A failed build must not be MEMOIZED, and a build that never SETTLES must
  // not be memoized either — the two halves of "a singleton build must reach a
  // terminal state", both discharged by the shared guard. A promise memo caches
  // a rejection as durably as a value, so without the clear one transient
  // failure leaves the event composer permanently unable to open a draft for
  // the page's life; and a wasm task that dies mid-poll never rejects at all,
  // so no `.catch` and no internal deadline can see it. The actor-scoped drop
  // saves neither (it runs on an identity CHANGE, not on the same actor
  // re-entering the route). `=== guarded` keeps the actor-changed throw above
  // from clearing a slot the drop has since refilled with the INCOMING actor's
  // build. Pinned by `singleton-build-memo-contract.test.ts`; mechanism in
  // `singleton-build.ts`.
  const guarded = guardSingletonBuild('event drafts', build, () => {
    if (facePromise !== guarded) return;
    facePromise = null;
    // Retract the face with the memo (`feed.ts` says why). `current` stays: it
    // is the user's live draft — every keystroke writes it — not this build's
    // product, and the replacement build's restore reseeds it.
    face = null;
  });
  facePromise = guarded;
  return guarded;
}

/** The draft the **New Event opener** should resume, or `null` when nothing is
 *  pending (`events.md` § Persistence — that opener restores; clearing there is
 *  what would make the rail inert).
 *
 *  **Non-destructive, and that is load-bearing.** An earlier consume-once version
 *  of this handed the draft to the first mount of the Events page and `null` to
 *  every mount after, so navigating away and back inside one page load showed an
 *  empty form for a draft the nest still held — caught by
 *  `test_event_draft_persistence.py[web]`, whose post-restart re-hydration visits
 *  Settings before returning to Events, i.e. the exact thing a returning user
 *  does. The caller is what decides whether resuming is safe (it declines when
 *  the compose already has authored text), not this accessor. */
export function resumeEventDraft(): EventDraft | null {
  if (!current) return null;
  const { summary, dtstart, dtend, description, location } = current;
  const empty = !summary && !dtstart && !dtend && !description && !location;
  return empty ? null : current;
}

// Under the Playwright e2e agent, debounce far shorter so a restart round-trip
// test (`test_event_draft_persistence.py`) needn't wait the production window —
// mirrors the identical E2E cadence in `feed.ts` / `conversations.ts`.
const DRAFT_SAVE_DEBOUNCE_E2E_MS = 150;
let draftSaveTimer: ReturnType<typeof setTimeout> | null = null;

function draftSaveDebounceMs(): number {
  const hasAgent =
    __FAUNA_E2E_AUTOMATION__ &&
    typeof window !== 'undefined' &&
    (window as unknown as { __faunaTestAgent?: unknown }).__faunaTestAgent != null;
  return hasAgent ? DRAFT_SAVE_DEBOUNCE_E2E_MS : autosaveDebounceMs();
}

/** Debounced persist of the event-composer draft after a compose change
 *  (`events.md` § Persistence). Coalesces a burst of keystrokes into one
 *  `fauna.drafts.put`; fire-and-forget (errors logged, never surfaced — a
 *  not-yet-synced draft is not a user-facing error). */
export function scheduleEventDraftSave(draft: EventDraft): void {
  // Update the resumable draft NOW, not when the debounce fires: a remount that
  // happens inside the debounce window must still see what the user typed.
  current = draft;
  if (draftSaveTimer) clearTimeout(draftSaveTimer);
  draftSaveTimer = setTimeout(() => {
    draftSaveTimer = null;
    if (!face) return;
    void face
      .saveDrafts(draft.summary, draft.dtstart, draft.dtend, draft.description, draft.location)
      .catch((e) => {
        logMessage('debug', 'fauna_web::events', `save event drafts failed (transient): ${e}`);
      });
  }, draftSaveDebounceMs());
}

/** Force an immediate save of the current held draft, bypassing the debounce —
 *  the leave-door flush (`reserved-folders.md` § The leave-flush promise, row
 *  481), called from the root layout's `visibilitychange`/`pagehide` handlers.
 *  Best-effort like every such handler on the web platform (the goal doc's own
 *  wording: a browser grants no reliable async work after either event) —
 *  mirrors `conversations.ts`/`feed.ts`'s twin, reading `current` directly since
 *  this rail has no manager to snapshot ([`scheduleEventDraftSave`]'s own doc). */
export function flushEventDraftNow(): void {
  if (draftSaveTimer) {
    clearTimeout(draftSaveTimer);
    draftSaveTimer = null;
  }
  if (!face || !current) return;
  const { summary, dtstart, dtend, description, location } = current;
  void face.saveDrafts(summary, dtstart, dtend, description, location).catch((e) => {
    logMessage('debug', 'fauna_web::events', `leave-flush event draft failed: ${e}`);
  });
}

/** Empty the rail — a created event, an explicit discard, or a day-cell "start a
 *  new event here" (`events.md` § Persistence). Distinct from clearing the
 *  page's own fields: forgetting this would leave a stale draft that reappears
 *  on the next launch for an event already on the calendar. */
export function clearEventDraft(): void {
  scheduleEventDraftSave({
    summary: '',
    dtstart: '',
    dtend: '',
    description: '',
    location: '',
  });
}

/** Tear down the singleton on an actor switch. Without it a soft-nav sign-out →
 *  sign-in-as-a-different-identity within one page load (module state survives)
 *  would keep saving under the PREVIOUS actor's `BackupKey` and could hand the
 *  new actor the previous one's restored draft — the same hazard
 *  `resetFeedManager` closes one rail over. A pending debounced save is dropped
 *  rather than flushed: it belongs to the outgoing actor.
 *
 *  This closes only the SYNCHRONOUS half. A `loadEventDrafts` still in flight
 *  writes `face` and `current` after this has run; the generation captured by
 *  its `sameActorSince()` is what stops it. */
export function resetEventDrafts(): void {
  if (draftSaveTimer) {
    clearTimeout(draftSaveTimer);
    draftSaveTimer = null;
  }
  face = null;
  facePromise = null;
  current = null;
}

// The drop registers itself, so no switch handler has to know this module exists
// (`actorScope.ts` — account-scoping.md § The scoping taxonomy, in-memory
// corollary). This module is only ever loaded when something actually opens the
// Events page, so a session that never does registers nothing.
registerActorScopedReset(resetEventDrafts);
