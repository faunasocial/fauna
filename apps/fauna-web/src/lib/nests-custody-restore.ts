// The Nests page's restore from a custodian nest (`nests.md` § Trust facet —
// restore from a custodian nest): the reading of `accountRestoreFromCustodian`'s
// answer, extracted from `NestsSection.svelte` so it is unit-testable without a
// live Svelte context. The copy and the form decision are shared Rust's
// (`CustodianRestoreOutcome::notice_parts` / `closes_form`, carried in the
// answer); this only resolves the parts against the generated `t` table —
// nested, joined by a space, the UniFFI apps' shape.
// Relative, not `#lib/...` — this module's own `.test.ts` runs under plain
// `deno test` (see `nests-generation.ts`).
import { resolveLocalizedNested, type LocalizedText } from './i18n/localized.ts';
import { t } from './i18n/strings.ts';

/** What a restore told the page: the notice to paint and whether the form
 *  closes, or `failed` — the gesture could not run at all, which belongs on
 *  the page's `error-message`, never the notice. */
export type RestoreReading =
  | { kind: 'notice'; text: string; closesForm: boolean }
  | { kind: 'failed'; reason: string };

/** Read the export's JSON answer. A malformed answer is a failure, never a
 *  silent drop (convention 11). */
export function readRestoreAnswer(json: string): RestoreReading {
  let answer: { notice?: LocalizedText[]; closes_form?: boolean; failed?: string };
  try {
    answer = JSON.parse(json);
  } catch {
    return { kind: 'failed', reason: `unreadable restore answer: ${json}` };
  }
  if (typeof answer.failed === 'string') return { kind: 'failed', reason: answer.failed };
  if (!Array.isArray(answer.notice)) {
    return { kind: 'failed', reason: `unreadable restore answer: ${json}` };
  }
  return {
    kind: 'notice',
    text: answer.notice.map((part) => resolveLocalizedNested(part)).join(' '),
    closesForm: answer.closes_form === true,
  };
}

/** The *running* state's notice — the surface's own state while the command
 *  is in flight, so it has no outcome. */
export const restoreRunningText = (): string => t.nests.custody_restore_running;
