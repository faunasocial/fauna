// Security-notice deadline args (`execute_after`, `lands_at`) arrive as Unix
// seconds; native apps render them as local time inside the shared
// `notification_text` (`local-clock`, off on the wasm leg), so web formats
// them here with the browser's own `Date` — `docs/goal/behavior/
// notifications.md` § Security notices, `value-formatting.md` § Absolute local
// timestamp display. Kept free of any `#lib/wasm` import so it is testable
// under plain deno.

import type { NotificationText } from './wasm';

/** The args the nest sends as Unix seconds (mirrors `DEADLINE_ARGS` in
 *  `fauna-client-notifications`'s `text.rs`). */
const DEADLINE_ARGS = ['execute_after', 'lands_at'];

/** Unix seconds → the browser's local date and time. */
export function epochSecsLocal(secs: number): string {
  return new Date(secs * 1000).toLocaleString();
}

/** A localized row with its numeric deadline args rewritten to local time;
 *  non-numeric deadline values, other args and verbatim rows pass through. */
export function withLocalDeadlines(text: NotificationText): NotificationText {
  if (text.kind !== 'localized' || !text.args) return text;
  const args: Record<string, string> = { ...text.args };
  for (const name of DEADLINE_ARGS) {
    const v = args[name];
    if (v !== undefined && /^-?\d+$/.test(v)) args[name] = epochSecsLocal(Number(v));
  }
  return { ...text, args };
}
