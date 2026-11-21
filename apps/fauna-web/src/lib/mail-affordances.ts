// How the mail pages paint their in-page affordances from the shared answers
// (`docs/goal/ui/mail-settings.md` § Errors & edge cases, *The in-page
// predicates*; `ui/README.md` § Offer only what is possible, rules 2, 5–7).
//
// Every answer is shared Rust read over wasm — the credential section's
// `WasmMailSettingsMachine.credentialManagementPossibility()`, the Outlook
// source's `WasmMailImportMachine.outlookSourcePossibility()`, the alias and
// list create's `createPossibility()` and the export Download's
// `carveOutPossibility(state)`. This module only maps an answer onto paint and
// onto a handler's refusal: the SPA never derives a mail condition of its own.
// It imports nothing that needs wasm or SvelteKit, so `deno test` can pin it
// (`mail-affordances.test.ts`).

import type { LocalizedText } from './i18n/localized.ts';
import { paintPossibility, type Possibility } from './possibility.ts';

/** One control's paint. `null` (not even the shared "checking" answer is
 *  readable yet — the wasm core is still loading) paints shown, disabled and
 *  with no reason, never live on a guess (rule 5). */
export function paintAnswer(
  answer: Possibility | null,
): { shown: boolean; disabled: boolean; reason: LocalizedText | null } {
  if (answer === null) return { shown: true, disabled: true, reason: null };
  return paintPossibility(answer);
}

/** The credential section's paint from
 *  `MailSettingsSnapshot::credential_management_possibility` (`null` before the
 *  first snapshot: nothing painted, as tui). Absent → no section; blocked →
 *  Add password shown, disabled, with its reason, and the rest of the section
 *  hidden; possible → the whole section live. */
export function credentialSectionPaint(answer: Possibility | null): {
  addShown: boolean;
  addDisabled: boolean;
  reason: LocalizedText | null;
  sectionShown: boolean;
} {
  if (answer === null) return { addShown: false, addDisabled: true, reason: null, sectionShown: false };
  const paint = paintPossibility(answer);
  return {
    addShown: paint.shown,
    addDisabled: paint.disabled,
    reason: paint.reason,
    sectionShown: answer.state === 'possible',
  };
}

/** The source kinds the picker offers: every kind of `kinds` — the shared
 *  `SOURCE_KINDS` list, read over wasm (`importSourceKinds()`) and passed in so
 *  this module stays wasm-free — except Outlook when the shared Outlook answer
 *  is absent (no Microsoft sign-in transport on this build — rule 2: what can
 *  never work here is not shown). Before the answer is readable (`null`) it is
 *  not known yet, so Outlook stays (rule 5). */
export function importSourceKindsOffered(kinds: readonly string[], outlook: Possibility | null): string[] {
  const outlookShown = outlook === null || outlook.state !== 'absent';
  return kinds.filter((k) => k !== 'Outlook' || outlookShown);
}

/** The reason a blocked answer carries (`null` for any other) — what a
 *  refusal writes on the page's `error-message`, e.g. the shared unbuilt
 *  answer `mailUsableUnbuilt()` a gesture with no machine refuses with
 *  (rule 7). */
export function blockedReason(answer: Possibility): LocalizedText | null {
  return answer.state === 'blocked' ? answer.reason : null;
}

/** What a gesture handler does with its control's answer, asked again at the
 *  press (rule 6): go on when possible; otherwise refuse, with the blocked
 *  reason for the page's `error-message` (an absent answer has none — its
 *  control is not painted, so nothing should have pressed it). `null` refuses
 *  with no reason: nothing is known yet. */
export function refusal(answer: Possibility | null): { refused: false } | { refused: true; reason: LocalizedText | null } {
  if (answer !== null && answer.state === 'possible') return { refused: false };
  return { refused: true, reason: answer?.state === 'blocked' ? answer.reason : null };
}
