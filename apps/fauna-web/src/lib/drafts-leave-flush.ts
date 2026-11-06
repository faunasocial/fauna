// The leave-door flush of every drafts rail (`reserved-folders.md` § The
// leave-flush promise): each rail's current compose saved now, bypassing its
// debounce. One body for both of web's leave doors, as tui's `LeaveFlush` is
// for its two:
//
// - **tab leave** (`visibilitychange` → hidden, `pagehide`; `RootShell.svelte`)
//   fires it and forgets it — a browser grants no reliable async work after
//   either event, so nothing there can wait on it;
// - **sign-out** (`identity.logout()`, `#lib/store`) AWAITS it before the
//   account runtime's stop, because that stop retires the session the save
//   rides and the actor-scope pass then drops the managers that hold the
//   snapshot: a flush issued after either could only lose the edit still
//   inside the debounce (the door `test_drafts_sign_out_flush.py` witnesses).
//
// Each rail is bounded on its own by the shared `LEAVE_FLUSH_BUDGET`, so a slow
// or unreachable nest never hangs signing out and one rail's timeout never
// costs another its save. A rail with nothing open is a no-op.

import { flushDraftsNow as flushConversationsDraftsNow } from './conversations';
import { flushDraftsNow as flushFeedDraftsNow } from './feed';
import { flushEventDraftNow } from './event-drafts';
import { leaveFlushBudgetMs, logMessage } from './wasm';

function bounded(rail: string, save: Promise<void>): Promise<void> {
  let lapse: ReturnType<typeof setTimeout> | undefined;
  const lapsed = new Promise<void>((resolve) => {
    lapse = setTimeout(() => {
      logMessage(
        'warn',
        'fauna_web::drafts',
        `leave-flush: the ${rail} rail did not save within its budget — going on without it`,
      );
      resolve();
    }, leaveFlushBudgetMs());
  });
  return Promise.race([save, lapsed]).finally(() => clearTimeout(lapse));
}

/** Save every drafts rail now, each bounded by the leave-flush budget. Never
 *  rejects: each rail logs its own failure. */
export async function flushDraftRailsNow(): Promise<void> {
  await Promise.all([
    bounded('conversations', flushConversationsDraftsNow()),
    bounded('posts', flushFeedDraftsNow()),
    bounded('events', flushEventDraftNow()),
  ]);
}
