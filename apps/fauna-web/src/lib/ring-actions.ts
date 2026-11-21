// The Logs pages' `log-copy-button` / `log-clear-button` (device Settings →
// Logs and the admin `admin-logs` page, `docs/goal/architecture/observability.md`
// § Surfaces; `docs/goal/ui/README.md` § Offer only what is possible, rules 2
// and 7). Whether either can do anything is shared Rust's
// `fauna_log::format::ring_action_possibility(shown, empty_key)` over its wasm
// twin `logRingActionPossibility`: blocked by the page's own empty line when the
// filter shows no entry. The paint and both handlers ask that one answer; the
// copy's outcome is confirmed or said on the page's `error-message`. The tui
// twins are `ring_action_possibility` / `record_copy_outcome`
// (`apps/fauna-tui/src/settings/logs.rs`) and `copy_button`
// (`apps/fauna-tui/src/admin/logs.rs`).
//
// Kept free of `#lib/wasm` (the `form-gate.ts` seam): the caller hands over the
// predicate call and the copy; `ring-actions.test.ts` pins it.
import { askPossibility, formRefusal } from './form-gate.ts';
import type { CopyOutcome } from './copy-outcome.ts';
import type { Possibility } from './possibility.ts';

/** The device page's empty line, the reason its ring actions are blocked. */
export const DEVICE_LOGS_EMPTY_KEY = 'logs.empty';
/** The admin page's empty line (the nest's ring). */
export const ADMIN_LOGS_EMPTY_KEY = 'admin.logs_page.empty';

/** What a copy leaves on the page: the string the clipboard took (painted as
 *  "Copied!" + `data-copied`), and the text for `error-message` (`''` none). */
export interface RingCopyResult {
  copied: string | null;
  error: string;
}

/** `log-copy-button`'s click: ask the answer again first — a refusal is said
 *  on `error-message` and nothing is copied; else copy and report. */
export async function copyRing(
  ask: () => Possibility,
  text: () => string,
  copy: (text: string) => Promise<CopyOutcome>,
): Promise<RingCopyResult> {
  const refusal = formRefusal(askPossibility(ask));
  if (refusal !== null) return { copied: null, error: refusal };
  const outcome = await copy(text());
  return outcome.failed === null
    ? { copied: outcome.copied, error: '' }
    : { copied: null, error: outcome.failed };
}

/** `log-clear-button`'s click: the refusal to say on `error-message`, or
 *  `null` once `clear` has run. */
export function clearRing(ask: () => Possibility, clear: () => void): string | null {
  const refusal = formRefusal(askPossibility(ask));
  if (refusal !== null) return refusal;
  clear();
  return null;
}
