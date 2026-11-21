// The Events page's write gates, as plain functions over the shared answers
// (`docs/goal/ui/events.md` § Errors & edge cases → *No calendar to hold an
// event* and *The other calendar writes*). The predicates themselves are shared
// Rust (`fauna_client_caldav::event_create_possibility` /
// `calendar_write_possibility`, through `#lib/wasm`); this module only turns
// their answer into what a door or an action says, so the page re-derives
// nothing and the unit tier can hold each door's behaviour without wasm.

import type { Possibility } from './possibility.ts';
import type { LocalizedText } from './i18n/localized.ts';

/** The predicate's "loading" reason (`events.create_blocked.loading`): what a
 *  write says when custody holds no key at the act — the mint is in flight,
 *  never a step for the person (ruling 5). */
export const LOADING_REASON: LocalizedText = { key: 'events.create_blocked.loading' };

/** The wasm calendar faces' refusal when custody holds no key at the act
 *  (`libs/fauna-wasm/src/rpc.rs`, every `dav_ctx(..)?.ok_or_else(..)`). The
 *  page never shows it: it is the key lost between the paint and the act. */
export const NO_KEY_REFUSAL = 'calendar requires mail to be enabled';

/** A door's or an action's answer to the predicate: `null` = go on; otherwise
 *  the reason to say on `error-message`. The predicates never answer absent;
 *  were one to, the action refuses as loading rather than send. */
export function refusalOf(answer: Possibility): LocalizedText | null {
  switch (answer.state) {
    case 'possible':
      return null;
    case 'blocked':
      return answer.reason;
    case 'absent':
      return LOADING_REASON;
  }
}

/** A thrown value's words: an `Error`'s message, or a rejected wasm string. */
export function errorText(e: unknown): string {
  if (typeof e === 'string') return e;
  if (e instanceof Error) return e.message;
  const message = (e as { message?: unknown } | null)?.message;
  return typeof message === 'string' ? message : String(e ?? '');
}

/** A calendar write's failure as what the page says: the no-key refusal is the
 *  predicate's "loading" (never "calendar requires mail"); anything else is its
 *  own words, or `fallback` when it has none. */
export function calendarWriteFailure(e: unknown, fallback: string): LocalizedText | string {
  const text = errorText(e);
  if (text.includes(NO_KEY_REFUSAL)) return LOADING_REASON;
  return text || fallback;
}

/** The unreadable-calendar notice for a read (rule 3: a calendar whose metadata
 *  will not unseal is left out and said, never only logged), or `null` when
 *  every listed calendar read. */
export function unreadableNotice(read: { state: string; unreadable?: number }): LocalizedText | null {
  const count = read.state === 'loaded' ? (read.unreadable ?? 0) : 0;
  if (count <= 0) return null;
  return { key: 'events.calendars_unreadable', args: { count: String(count) } };
}
