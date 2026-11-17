// How a mail sub-page and its settings rail row paint the shared mail-usable
// answer (`docs/goal/ui/mail-settings.md` § Errors & edge cases, the
// mail-usable gate; `ui/README.md` § Offer only what is possible, rules 2–7).
//
// The answer itself is shared Rust — `MailSettingsSnapshot::mail_usable()`,
// read over wasm as `WasmMailSettingsMachine.mailUsable()` (or
// `mailUsableUnbuilt()` when the machine failed to build; `mail-usable.ts`).
// This module only maps it onto paint: the SPA never derives a mail condition
// of its own.

import type { LocalizedText } from '#lib/i18n/localized';

import { paintPossibility, type Possibility } from './possibility.ts';

/** The shared answer type lives in `#lib/possibility`; re-exported for the
 *  mail callers that already import it from here. */
export type { Possibility };

/** The rail ids of the six gated mail sub-pages, in rail order. */
export const MAIL_SUBPAGES: readonly string[] = [
  'mail-aliases',
  'mail-spam',
  'mail-export',
  'mail-import',
  'mail-lists',
  'mail-list-members',
];

export function isMailSubpage(id: string): boolean {
  return MAIL_SUBPAGES.includes(id);
}

/** The rail row's paint: absent → not shown; blocked → disabled with the
 *  reason on the row; possible → live. `null` (the wasm core not loaded yet, so
 *  not even the shared "checking" answer is readable) → disabled, no reason. */
export function railRowGate(
  answer: Possibility | null,
): { shown: boolean; disabled: boolean; reason: LocalizedText | null } {
  if (answer === null) return { shown: true, disabled: true, reason: null };
  return paintPossibility(answer);
}

/** Whether a mail sub-page shows only its heading, a text and Back in place of
 *  its content, and that text — the blocked reason, or (reached by another door
 *  on a box that can never serve mail) the Mail & Calendar page's own
 *  unavailable notice. Not gated when possible: the page renders as built.
 *  `null` (nothing readable yet) is gated with no text. */
export function subpageGate(answer: Possibility | null): { gated: boolean; text: LocalizedText | null } {
  if (answer === null) return { gated: true, text: null };
  switch (answer.state) {
    case 'possible':
      return { gated: false, text: null };
    case 'blocked':
      return { gated: true, text: answer.reason };
    case 'absent':
      return { gated: true, text: { key: 'mail_settings.unavailable_notice' } };
  }
}
