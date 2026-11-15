// The unsent-changes surface's re-read edge (`account-offline-mutation.md`
// § The offline-mutation contract → *The unsent-changes surface*): the
// runtime's generic outbox drain tells the app nothing when it parks or sends
// an intent, so each surface that shows unsent state — the feed heart, an open
// conversation's queued own copies, the Settings → Account list — re-reads this
// device's outbox on a short tick while it is painted (tui's
// `session::spawn_unsent_refresh`). Every read is local: no nest is asked.

/** How often a painted unsent-state surface re-reads the outbox (tui's tick). */
export const UNSENT_REREAD_MS = 2000;

/** Run `reread` every `UNSENT_REREAD_MS` until the returned stop is called —
 *  for a surface's `onMount` (return the stop, or call it from `onDestroy`).
 *  A pass still in flight is never overlapped by the next; a failed pass is
 *  dropped (the next tick reads again). */
export function startUnsentTick(reread: () => Promise<unknown> | unknown): () => void {
  let busy = false;
  const timer = setInterval(async () => {
    if (busy) return;
    busy = true;
    try {
      await reread();
    } catch {
      // A failed re-read keeps the last paint; the next tick reads again.
    } finally {
      busy = false;
    }
  }, UNSENT_REREAD_MS);
  return () => clearInterval(timer);
}
