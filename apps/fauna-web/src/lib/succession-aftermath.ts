// The post-succession aftermath's progress, as the SPA holds it — web's twin
// of tui's/linux's in-memory `AftermathProgress`
// (the render
// layer itself is on the Settings → Recovery kit
// page).
//
// **Why a module store rather than page state.** The pass is *started* from
// the root layout, at the actor settle that follows a successor's sign-in; its
// lines are *read* on the Settings → Recovery kit sub-page, which the user
// reaches later and which mounts fresh. Page-local state would therefore be
// null at exactly the moment the surface exists to be read — and the aftermath
// journeys assert precisely that read, deliberately without visiting the pages
// that would heal what they came to measure. A native app gets this for free
// from one process-wide `App`; this module is web's equivalent.
//
// **Nothing here is at rest, and that is the design.** The corpus is its own
// progress record (each leg is idempotent and re-runs at the next sign-in), so
// these lines are a report about *this* session's pass and nothing more. They
// are dropped on an actor switch with every other piece of actor-scoped state.

import { writable } from 'svelte/store';
import { registerActorScopedReset } from './actorScope';
import type { LocalizedText } from './i18n/localized';

/** One line per leg, in the order the legs run — each already resolved to a
 *  `LocalizedText` by the shared projection that owns its copy, never composed
 *  here. `null` is a value, not an absence: a leg that finished owing the user
 *  nothing to read reports it, and the render hides that line. */
export interface AftermathProgress {
  /** Leg 2 — the `NestBackupKey` re-grant. */
  backupRegrant: LocalizedText | null;
  /** Leg 3 — the `__mls` re-seal. **Filled as of 2026-08-21**; it was null here for a week while the plane underneath
   *  it already worked, which is why this comment is explicit about the two
   *  halves being separate:
   *
   *  - the PASS runs because `MlsStateSync::with_predecessors(..)` is wired at
   *    `libs/fauna-wasm/src/conversations.rs` — a web successor's
   *    conversations really do come back;
   *  - the LINE arrives because `.with_reseal_sink(..)` is wired beside it, and
   *    that sink dispatches through a thread-local rather than closing over the
   *    callback: `ResealSink` is `Send + Sync` and a `js_sys::Function` is not.
   *
   *  ⚠ Unlike every other leg here, this one does NOT report from
   *  `runSuccessionAftermath`. It is a barrier inside the conversations
   *  replica's own load, so its sink is registered in `$lib/conversations`
   *  BEFORE the manager is built — registering it later misses the pass. */
  mlsReseal: LocalizedText | null;
  /** Leg 4 — the capability-grant re-mint. */
  grantRemint: LocalizedText | null;
  /** Leg 5 — the file-corpus re-seal. Stays null on every app in this store:
   *  the only aftermath arm whose work does not run in the app process at all
   *  (it is the sync agent's), so it reports from where the work happens. */
  corpusReseal: LocalizedText | null;
  /** Leg 7 — the `__drafts` re-seal. Rendered ABOVE leg 6, like tui's. */
  draftsReseal: LocalizedText | null;
  /** Leg 6 — the mail burn, the only leg that takes something away. */
  mailBurn: LocalizedText | null;
}

function nothingReported(): AftermathProgress {
  return {
    backupRegrant: null,
    mlsReseal: null,
    grantRemint: null,
    corpusReseal: null,
    draftsReseal: null,
    mailBurn: null,
  };
}

/** The lines the Recovery kit section renders. */
export const aftermathProgress = writable<AftermathProgress>(nothingReported());

/** The legs the wasm pass reports on — the callback's `leg` vocabulary, which
 *  is deliberately this interface's own field names so a leg files itself with
 *  no second match table on this side. */
type ReportingLeg = keyof AftermathProgress;

const REPORTING_LEGS: readonly ReportingLeg[] = [
  'backupRegrant',
  'mlsReseal',
  'grantRemint',
  'corpusReseal',
  'draftsReseal',
  'mailBurn',
];

/** Bumped each time the post-store-ready pass settles — the shared driver's
 *  `config_stage_settled`, which fires once the two silent raises have had
 *  their turn on the succession ledger. The review surfaces re-read their
 *  marks on it, as linux's and apple's sinks do at the same hook. It carries
 *  no line, so it is not a leg. */
export const configStageSettled = writable(0);
const CONFIG_STAGE_SETTLED = 'configStageSettled';

/** File one leg's line, as `runSuccessionAftermath`'s progress callback hands
 *  it over. An unknown leg is logged and dropped rather than written blindly:
 *  the wasm build and this file always ship together, so a mismatch is a bug
 *  worth seeing in the console ring, never a reason to break the pass. */
export function recordAftermathLeg(leg: string, line: LocalizedText | null): void {
  if (leg === CONFIG_STAGE_SETTLED) {
    configStageSettled.update((n) => n + 1);
    return;
  }
  if (!(REPORTING_LEGS as readonly string[]).includes(leg)) {
    console.warn('[succession] aftermath reported an unknown leg:', leg);
    return;
  }
  aftermathProgress.update((current) => ({ ...current, [leg as ReportingLeg]: line }));
}

// Actor-scoped: a switch drops the previous identity's report before the new
// identity's handlers run, so a successor never reads the account it left.
registerActorScopedReset(() => aftermathProgress.set(nothingReported()));
