// The Contacts page's knock accept (`account-offline-mutation.md` § The
// offline-mutation contract, and its § Implementation status today entry for
// the knock-accept and abuse-report composers). This module stays wasm-free so
// its routing is testable under plain `deno test` (`knock-accept.test.ts`); the
// page passes the real rpc.ts / api.ts calls.

/** What the accept calls: the composer's send-or-queue (`true` = queued) and
 *  the page's re-read after a sent accept. */
export interface AcceptKnockVerbs {
  acceptOrQueue: () => Promise<boolean>;
  reread: () => Promise<void>;
}

/**
 * Accept one knock. Sent → the knocks and roster are re-read (`'sent'`).
 * Queued with no nest → `'queued'`: the caller drops the knock from its local
 * list and adds no roster row — the nest's read after the drain is the word
 * (tui's rule).
 */
export async function acceptKnock(verbs: AcceptKnockVerbs): Promise<'sent' | 'queued'> {
  if (await verbs.acceptOrQueue()) return 'queued';
  await verbs.reread();
  return 'sent';
}
