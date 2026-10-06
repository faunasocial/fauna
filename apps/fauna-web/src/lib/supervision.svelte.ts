// Web's restore call site for the persisted last-known supervision snapshot
// (family-safety.md § Content policy, clause 2) — the web twin of tui's
// `restore_supervision_snapshot` (`apps/fauna-tui/src/family.rs`, wired in
// `session.rs::establish` ahead of `spawn_status_check`).
//
// Three call sites, no design (the ruling's words): the WRITE is the
// `familyStatus` read's own success path in Rust (`libs/fauna-wasm/src/rpc.rs`
// — web's one choke point for the read), the RESTORE is here (the layout runs
// it when the identity becomes known, ahead of the first read landing), and
// the ERASE rides the registry's `remove`/`clear_all` with the account's other
// stores. The decision of what a snapshot seeds is the pure
// `applyRestoredSnapshot` (`supervisionRestore.ts`, deno-pinned); this module
// only holds the restored-guardian `$state` the layout's supervised indicator
// and `family-tab` gate fall back to until a live read supersedes it.

import { accountsSupervisionSnapshot } from './accounts';
import { applyRestoredSnapshot } from './supervisionRestore';
import { setGuardianHalf } from './contentPolicy.svelte';
import { setWardScreenTime } from './screenTime.svelte';
import { registerActorScopedReset } from './actorScope';

// The guardian of the RESTORED supervision, `null` when nothing is restored —
// or once a live read supersedes it (`clearRestoredSupervision`). Kept apart
// from the layout's live `famStatus` so "last-known" and "just read" can never
// be confused for each other.
let restoredGuardianHandle = $state<string | null>(null);
// Whether a successful read has landed for the current actor. Restore is
// async (a wasm hop) and the first read races it; when the read wins, a
// late-landing restore must become a no-op — re-seeding the stores with
// last-known values OVER a fresh reply would be exactly the stale render the
// snapshot exists to prevent, in the other direction.
let liveReadLanded = false;

/** The restored guardian's handle, for the supervised indicator + `family-tab`
 *  gate to fall back to while no live read has landed. */
export function restoredGuardian(): string | null {
  return restoredGuardianHandle;
}

/** A successful `familyStatus` read landed — the live reply is the truth now
 *  (and the wasm choke point has already re-persisted it), so the restored
 *  fallback must stop answering: without this, a read that says UNSUPERVISED
 *  would leave a stale restored guardian driving the indicator. */
export function clearRestoredSupervision(): void {
  restoredGuardianHandle = null;
  liveReadLanded = true;
}

/** Restore `actorId`'s last-known supervision at launch, ahead of the first
 *  read (clause 2's whole point: a supervised ward who launches offline keeps
 *  their floor — and their bedtime lock, which is pure local clock — instead
 *  of rendering unsupervised until a read succeeds). */
export async function restoreSupervisionSnapshot(actorId: string): Promise<void> {
  const snap = await accountsSupervisionSnapshot(actorId);
  if (liveReadLanded) return;
  restoredGuardianHandle = applyRestoredSnapshot(snap, {
    restoreContentPolicy: setGuardianHalf,
    restoreScreenTime: (policy, guardianHandle) =>
      setWardScreenTime(policy, guardianHandle, undefined),
  });
}

// Class-1 account-scoped state: the restored guardian must not survive into
// the next account's render (`account-scoping.md` § The scoping taxonomy) —
// and the incoming actor gets a fresh restore-vs-read race, so the
// live-read flag resets with it. The content/screen-time halves register
// their own drops where they live.
registerActorScopedReset(() => {
  restoredGuardianHandle = null;
  liveReadLanded = false;
});
