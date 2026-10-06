// The supervised ward's OWN outstanding asks, off `fauna.family.status` — the
// durable half of the contacts / profile / bridges ask surfaces
// (family-safety.md § Child-initiated contact requests → *Ward transparency*,
// § Feed-source approvals). The web twin of tui's
// `FamilyState::{own_contact_requests, own_feed_requests}`: one shared cache
// every page reads, fed by every successful status read (the root layout's,
// the Family page's, and each ask surface's own mount / post-ask re-read), so
// "asked — waiting" survives navigation and a reload instead of living in a
// page-local flag. The decision rules are the pure `ward-asks.ts`.
//
// A `.svelte.ts` module so the cache is `$state`: a page reading it in its
// template re-renders when a late status read lands.

import { familyStatus, type FamilyStatus } from '$lib/rpc';
import { registerActorScopedReset } from '$lib/actorScope';
import { keepOnEmptyReread, wardAsksFromStatus, type WardAsks } from '$lib/ward-asks';

let asks = $state<WardAsks>({ contact: [], feed: [] });

/** The current cache (reactive when read inside a component). */
export function wardAsks(): WardAsks {
  return asks;
}

/** Move the cache to what a SUCCESSFUL status read says — gated on
 *  `supervised_by` inside `wardAsksFromStatus` (a graduated account keeps no
 *  stale ask). A failed read never calls this: no information, keep the last. */
export function setWardAsksFromStatus(status: FamilyStatus): void {
  asks = wardAsksFromStatus(status);
}

/** Best-effort status read for an ask surface's mount — so the durable state
 *  paints on a fresh open without waiting on another page's read. */
export async function refreshWardAsks(secretHex: string): Promise<void> {
  try {
    setWardAsksFromStatus(await familyStatus(secretHex));
  } catch {
    // No information — keep what the last successful read established.
  }
}

/** The re-read after an ask landed. The ask's own reply is a bare ack, so
 *  without this the pending state would not land until the next status read.
 *  A failed (or empty) re-read is NOT a failed ask — the guardian has been rung
 *  — so it never wipes what the cache already holds; the caller's just-asked
 *  flag carries the render meanwhile. */
export async function rereadAfterAsk(secretHex: string): Promise<void> {
  try {
    const fresh = wardAsksFromStatus(await familyStatus(secretHex));
    asks = {
      contact: keepOnEmptyReread(asks.contact, fresh.contact),
      feed: keepOnEmptyReread(asks.feed, fresh.feed),
    };
  } catch {
    // Keep the held lists; the local flag carries the render.
  }
}

// Class-1 account-scoped state (`account-scoping.md` § The scoping taxonomy):
// one ward's asks must never render for the next account.
registerActorScopedReset(() => {
  asks = { contact: [], feed: [] };
});
