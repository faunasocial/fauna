// The Privacy filter form's create (`account-offline-mutation.md` § The
// offline-mutation contract, and its § Implementation status today entry for
// the email filter-create composer). This module stays wasm-free so its
// routing is testable under plain `deno test` (`email-filter-create.test.ts`);
// the page passes the real rpc.ts / api.ts calls.

import type { EmailFilter } from './rpc.ts';

/** What the create calls: the composer's send-or-queue (`true` = queued) and
 *  the list re-read after a sent create. */
export interface CreateFilterVerbs {
  createOrQueue: () => Promise<boolean>;
  reread: () => Promise<EmailFilter[]>;
}

/**
 * Create one filter rule. Sent → the re-read list. Queued with no nest →
 * `null`: the form resets like a create, but nothing is listed or re-read — the
 * rule has no server id until the drain, and the nest's list afterwards is the
 * word (tui's rule).
 */
export async function createFilter(verbs: CreateFilterVerbs): Promise<EmailFilter[] | null> {
  if (await verbs.createOrQueue()) return null;
  return verbs.reread();
}
