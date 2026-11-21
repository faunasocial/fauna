// A copy button's outcome, kept apart from the clipboard itself so a page can
// say what happened (`docs/goal/ui/README.md` § Offer only what is possible,
// rule 7 — a failure that slips through is never silent): a write that reached
// the clipboard confirms ("Copied!" + the `data-copied` value, the
// `account-actor-id-copy-btn` contract); one that failed puts the shared
// `common.copy_failed` sentence on the page's error line. The tui twin is
// `record_copy_outcome` (`apps/fauna-tui/src/settings/logs.rs`).
//
// Kept free of `#lib/wasm` so plain `deno test` can drive it
// (`copy-outcome.test.ts`); `copy-confirm.ts` adds the log-ring record.
import { t } from './i18n/strings.ts';

export type CopyOutcome = { copied: string; failed: null } | { copied: null; failed: string };

/** The writer a copy goes through — the browser's async clipboard by default. */
export type ClipboardWrite = (text: string) => Promise<void>;

const browserWrite: ClipboardWrite = (text) => navigator.clipboard.writeText(text);

/** Write `text` and report: `copied` is the exact string the clipboard took,
 *  `failed` the localized `common.copy_failed` sentence. A writer that throws
 *  synchronously (no clipboard at all) fails the same way. */
export async function copyOutcome(text: string, write: ClipboardWrite = browserWrite): Promise<CopyOutcome> {
  try {
    await write(text);
    return { copied: text, failed: null };
  } catch (e) {
    const error = e instanceof Error ? e.message : String(e);
    return { copied: null, failed: t.common.copy_failed({ error }) };
  }
}
