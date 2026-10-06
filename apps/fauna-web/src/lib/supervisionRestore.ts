// The pure half of web's supervision-snapshot restore (family-safety.md
// § Content policy, clause 2 — the persisted last-known supervision snapshot).
//
// `supervision.svelte.ts` is the stateful wiring; this module is the decision:
// given the snapshot the registry slot yielded (already parsed by its single
// format owner, `fauna_client_family::SupervisionSnapshot`, over the wasm
// boundary), which enforcement inputs get seeded, with what. Kept as a plain,
// sink-injected function so the deno unit tests can pin the fold-out the way
// linux pins its pure producer — the store-backed halves are carried by the
// shared crates' own tests.

import type { ContentPolicyValue, ScreenTimePolicyValue } from './wasm';

/** The JS shape of `fauna_client_family::SupervisionSnapshot` as
 *  `accountsSupervisionSnapshot` yields it (serde `json_compatible`: absent
 *  options land as `null`). */
export interface SupervisionSnapshotValue {
  supervised_by: { actor_id_hex: string; handle: string } | null;
  content_policy: ContentPolicyValue | null;
  content_notify: boolean;
  screen_time: ScreenTimePolicyValue | null;
}

/** Where a restored snapshot's three client-enforced pillars land. */
export interface RestoreSinks {
  /** The guardian content floor + the Guardian Notify knob
   *  (`contentPolicy.svelte.ts`). */
  restoreContentPolicy(policy: ContentPolicyValue | null, notify: boolean): void;
  /** The screen-time policy + the guardian the lock names
   *  (`screenTime.svelte.ts`; usage stays unheard — the ratified fail-open on
   *  the budget arm, it re-arrives with the first successful read). */
  restoreScreenTime(policy: ScreenTimePolicyValue | null, guardianHandle: string): void;
}

/**
 * Seed the enforcement inputs from a restored snapshot; returns the guardian
 * handle for the supervised indicator + `family-tab` gate, or `null` when
 * nothing is restored.
 *
 * An absent snapshot is "no information" and an unsupervised one is the
 * positive fact "this account was unsupervised as of the last successful
 * read" — in both cases NOTHING is seeded, deliberately through the same
 * early return: the stores already hold the unsupervised state, and seeding
 * any field off a guardian-less snapshot would be the graduation bug the
 * shared fold's gate exists to prevent (a floor may only ever be restored
 * under the guardianship that owns it).
 */
export function applyRestoredSnapshot(
  snap: SupervisionSnapshotValue | undefined,
  sinks: RestoreSinks,
): string | null {
  if (!snap?.supervised_by) return null;
  sinks.restoreContentPolicy(snap.content_policy, snap.content_notify);
  sinks.restoreScreenTime(snap.screen_time, snap.supervised_by.handle);
  return snap.supervised_by.handle;
}
