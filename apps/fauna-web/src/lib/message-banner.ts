// The running tab's new-message OS banner — `conversations` outcome 11, the
// web leg.
//
// Split exactly as `docs/goal/ui/conversations.md` § Where logic lives rules it:
// the *when / for-whom* decision is the shared `MessageNotificationTracker`,
// reached here as one call (`manager.newMessageBanners()`) and re-derived
// nowhere in TS; the *firing* — one `new Notification()` per entry — is this
// app's glue, the browser twin of linux's freedesktop call and windows' WinUI
// toast. The three rules (seed silently on the first non-empty snapshot, fire
// on new activity, suppress the thread you have open) are the WHOLE decision:
// a fourth rule added here would be divergence, not refinement.
//
// **This is the RUNNING tab.** The service worker's `showNotification` covers
// the opposite case — a push arriving with the app closed
// (`src/service-worker.ts`) — and the two never overlap.
//
// **Never keyed on the push "subscribed" bit.** `docs/goal/ui/settings.md`
// § Push notifications rules those two apart deliberately: the bit is the user's
// opt-in for THIS install, and a successful Disable leaves the platform
// permission granted. Keying the running tab's banner on it would go silent for
// exactly the user who switched off background push while sitting in front of
// the app, which is not what they asked for.
//
// **Nor on `Notification.permission`** — the browser is the one that decides
// whether a banner appears, and it already does: the constructor is a silent
// no-op when permission is `denied` or `default`, and it raises no prompt (only
// `requestPermission()` does, and that gesture belongs to the settings page, not
// to a message's arrival). Reading the permission first would add a *fourth*
// when/for-whom rule in app glue, which is the divergence
// `conversations.md` § Where logic lives forbids and linux was corrected for on
// 2026-09-20 — and it would key the fired log on a platform setting rather than
// on what this app did, diverging from linux, which records its freedesktop call
// without asking whether a daemon is listening. So: hand every decided banner to
// the browser and record it. Whether the user sees it is the last inch (their
// permission, their do-not-disturb), not the mechanism.

import type { WasmConversationsManager } from '../../static/fauna_wasm.js';

/** One banner this tab actually fired, in firing order. */
export interface FiredBanner {
  thread_id: string;
  label: string;
}

// The fired-banner log + its two diff-tick counters — web's leg of
// `fauna_e2e_agent::MESSAGE_BANNERS_KEY`, read out through
// `$lib/e2e-automation`'s `__fauna_messageBanners` hook (the hook is what the
// production bundle drops; these three are plain module state, the same shape
// the receive pump's counters already have in `$lib/conversations`).
//
// The counters are not decoration: two of the three rules are NEGATIVE, and a
// tick that merely *finished* after a test planted its message may have read the
// snapshot before it. `started` is bumped before the diff and `completed` after
// the last fire, so `completed > started_at_plant` proves by pigeonhole that a
// tick which began after the plant has finished.
let started = 0;
let completed = 0;
const fired: FiredBanner[] = [];

/** `{started, completed, fired}` — read by the e2e bridge's state assembly. */
export function messageBanners(): { started: number; completed: number; fired: FiredBanner[] } {
  return { started, completed, fired: [...fired] };
}

/**
 * Run one banner diff tick against the live conversations manager and fire a
 * system notification for each thread it returns.
 *
 * Call exactly once per snapshot change, from the post-change chokepoint
 * (`refreshConversations`) — the tracker is stateful, so a discarded tick is a
 * banner the user never sees, and a doubled tick is one the tracker has already
 * accounted for.
 *
 * Takes the manager rather than reaching for the singleton itself: `$lib/
 * conversations` owns that singleton and calls this, and the reverse import
 * would be a cycle (the same shape `syncFeedRoomPosts` keeps with `$lib/feed`).
 */
export function raiseNewMessageBanners(manager: WasmConversationsManager): void {
  started += 1;
  try {
    const rows = manager.newMessageBanners() as Array<{
      threadId: string;
      label: string;
      snippet: string;
    }>;
    for (const row of rows) {
      try {
        // `tag` collapses a rapid run from one thread into a single banner
        // rather than a stack — the browser twin of linux's replace-id.
        new Notification(row.label, { body: row.snippet, tag: row.threadId });
      } catch (e) {
        // A browser with no Notification constructor at all (an iOS Safari tab
        // outside a PWA, a restrictive policy) has no banner surface for this
        // message — and nothing was handed over, so nothing is recorded. The
        // rest of the tick stands.
        console.warn('message banner failed:', e);
        continue;
      }
      fired.push({ thread_id: row.threadId, label: row.label });
    }
  } catch (e) {
    console.warn('message banner diff failed:', e);
  } finally {
    // In a `finally` so a thrown diff still closes its tick: a barrier that only
    // advanced on the happy path would hang a negative assertion instead of
    // failing it (`fauna_e2e_agent::ACTIVATION_GESTURES_KEY` makes the same
    // argument for the same reason).
    completed += 1;
  }
}

/** Forget this tab's log — the actor-switch twin of the manager rebuild that
 *  gives the shared tracker a fresh seed (`resetConversationsManager`). */
export function resetMessageBanners(): void {
  started = 0;
  completed = 0;
  fired.length = 0;
}
