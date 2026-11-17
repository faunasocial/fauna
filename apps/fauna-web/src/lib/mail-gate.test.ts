// Deno test for the mail-usable gate's paint (`mail-gate.ts`;
// `docs/goal/ui/mail-settings.md` § Errors & edge cases, the mail-usable gate).
// Run via:
//
//     deno test apps/fauna-web/src/lib/mail-gate.test.ts

import { isMailSubpage, MAIL_SUBPAGES, type Possibility, railRowGate, subpageGate } from './mail-gate.ts';

function eq<T>(actual: T, expected: T, msg: string) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${msg}\n  expected: ${e}\n  actual:   ${a}`);
}

const off: Possibility = { state: 'blocked', reason: { key: 'mail_settings.usable_needs_mail_on' } };

Deno.test('the six mail sub-pages are the gated ones, and nothing else', () => {
  eq(MAIL_SUBPAGES.length, 6, 'six pages');
  eq(isMailSubpage('mail-aliases'), true, 'aliases gated');
  eq(isMailSubpage('mail-list-members'), true, 'list members gated');
  eq(isMailSubpage('mail-settings'), false, 'the Mail & Calendar page stays live: it is the one step');
  eq(isMailSubpage('archive-import'), false, 'archive import is not a mail sub-page');
});

Deno.test('a rail row is disabled with the shared reason while mail is off', () => {
  eq(railRowGate(off), { shown: true, disabled: true, reason: off.reason }, 'blocked row');
});

Deno.test('a rail row is not shown on a box that can never serve mail', () => {
  eq(railRowGate({ state: 'absent' }).shown, false, 'absent row');
});

Deno.test('a rail row is live once mail is usable', () => {
  eq(railRowGate({ state: 'possible' }), { shown: true, disabled: false, reason: null }, 'possible row');
});

Deno.test('a sub-page opened while not possible shows the reason in place of its content', () => {
  eq(subpageGate(off), { gated: true, text: off.reason }, 'blocked page');
  eq(
    subpageGate({ state: 'absent' }),
    { gated: true, text: { key: 'mail_settings.unavailable_notice' } },
    'absent page',
  );
  eq(subpageGate({ state: 'possible' }), { gated: false, text: null }, 'possible page renders as built');
});

// Before the wasm core loads not even the shared "checking" answer
// (`mailUsableUnread()`) is readable: the gate stays closed and the SPA spells
// no reason of its own.
Deno.test('nothing readable yet: rows disabled and sub-pages gated, with no reason the SPA made up', () => {
  eq(railRowGate(null), { shown: true, disabled: true, reason: null }, 'unread row');
  eq(subpageGate(null), { gated: true, text: null }, 'unread page');
});
