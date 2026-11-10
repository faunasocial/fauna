// The running tab's knock toast — web's leg of
// `docs/goal/behavior/notifications.md` § Localized body, *The knock toast*
// ("every app raises an OS-level toast for an inbound knock"), the twin of
// tui's `App::raise_knock_toast` and linux's `notifications::raise_knock_toast`.
//
// Split as the goal doc rules it: both decisions — the muted-word gate
// (§ Muted words rule 6) and the toast's text — are the shared
// `fauna_client_notifications::knock_toast`, reached as one wasm call and
// re-derived nowhere in TS; the firing — a browser `Notification` titled
// `notifications.knock_title` — is this app's glue.
//
// **One tag, derived from the shared replace id.** Every knock toast carries
// the tag `fauna-knock-<KNOCK_TOAST_REPLACE_ID>`, so the browser replaces one
// notice for a flood of fresh senders instead of stacking — the tag is web's
// replace key, one-to-one with the shared 1003 the natives raise under, and the
// log records that id.
//
// **Not keyed on `Notification.permission`**, for the reason
// `#lib/message-banner` gives: the browser decides whether the notice appears
// (a silent no-op without permission), and a permission read here would key
// the log on a platform setting rather than on what this app did. Whether the
// user sees it is the last inch.
//
// The log (`fauna_e2e_agent::KNOCK_NOTICES_KEY`) is the shared `KnockNotices`
// in the core wasm chunk's test-helpers build: the push is counted before the
// gate, the toast recorded after it was handed over.

import { onPushEvent } from './rpc';
import {
  knockNoticesObserveForTest,
  knockNoticesRecordForTest,
  knockToast,
  knockToastReplaceId,
} from './wasm';
import { heldMutedWords } from './muted-words';
import { resolveLocalized } from './i18n/localized';
import { t } from './i18n/strings.ts';

/** Raise the knock toast for one `fauna.knock` push payload. */
export function raiseKnockToast(payload: unknown): void {
  knockNoticesObserveForTest();
  let text;
  try {
    text = knockToast(payload, heldMutedWords());
  } catch (e) {
    console.warn('knock toast decision failed:', e);
    return;
  }
  if (!text) return; // a muted word withholds it (rule 6)
  const key = text.kind === 'localized' ? text.key : null;
  const body = text.kind === 'localized' ? resolveLocalized(text) : text.text;
  const replaceId = knockToastReplaceId();
  try {
    new Notification(t.notifications.knock_title, { body, tag: `fauna-knock-${replaceId}` });
  } catch (e) {
    // No Notification constructor in this browser: nothing was handed over,
    // so nothing is recorded (`#lib/message-banner`'s rule).
    console.warn('knock toast failed:', e);
    return;
  }
  knockNoticesRecordForTest(key, body, replaceId);
}

/** Subscribe the app-wide knock arm for as long as a session is signed in;
 *  returns the unsubscribe. Registered once by `RootShell`, never per page —
 *  a knock must ring whichever page the user is on. */
export function subscribeKnockToasts(): () => void {
  return onPushEvent((kind, payload) => {
    if (kind === 'fauna.knock') raiseKnockToast(payload);
  });
}
