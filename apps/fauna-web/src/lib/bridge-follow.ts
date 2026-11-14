// A bridge follow or unfollow (`account-offline-mutation.md` § The
// offline-mutation contract, and its § Implementation status today entry for
// the bridges follow composer). This module stays wasm-free so its routing is
// testable under plain `deno test` (`bridge-follow.test.ts`); the pages pass
// the real `bridges.ts` calls.

/**
 * Send one follow-list change, or — with no nest — let the shared composer
 * queue it (`sendOrQueue` resolves `true` when it QUEUED). Sent → the page
 * re-reads its list. Queued → nothing is re-read and no list changes: the
 * bridges pages paint only what the nest persisted, and its list after the
 * drain is the word (tui's rule); the caller clears its page error.
 * Resolves `true` when queued.
 */
export async function changeFollow(
  sendOrQueue: () => Promise<boolean>,
  reread: () => Promise<unknown>,
): Promise<boolean> {
  if (await sendOrQueue()) return true;
  await reread();
  return false;
}
