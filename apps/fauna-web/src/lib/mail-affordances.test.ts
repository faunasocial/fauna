// Deno test for the mail pages' in-page affordance paint (`mail-affordances.ts`;
// `docs/goal/ui/mail-settings.md` § Errors & edge cases, *The in-page
// predicates*). Run via `just web-unit-test`.

import {
  blockedReason,
  credentialSectionPaint,
  importSourceKindsOffered,
  paintAnswer,
  refusal,
} from './mail-affordances.ts';
import type { Possibility } from './possibility.ts';

function eq<T>(actual: T, expected: T, msg: string) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${msg}\n  expected: ${e}\n  actual:   ${a}`);
}

const mailOff: Possibility = { state: 'blocked', reason: { key: 'mail_settings.credentials_need_mail_on' } };
const noDomain: Possibility = { state: 'blocked', reason: { key: 'mail_aliases.no_default_domain' } };
const checking: Possibility = { state: 'blocked', reason: { key: 'common.still_loading' } };

Deno.test('credential section: absent paints nothing', () => {
  eq(
    credentialSectionPaint({ state: 'absent' }),
    { addShown: false, addDisabled: true, reason: null, sectionShown: false },
    'absent',
  );
});

Deno.test('credential section: blocked shows only Add password, disabled with its reason', () => {
  eq(
    credentialSectionPaint(mailOff),
    { addShown: true, addDisabled: true, reason: mailOff.reason, sectionShown: false },
    'blocked',
  );
});

Deno.test('credential section: possible is the whole live section', () => {
  eq(
    credentialSectionPaint({ state: 'possible' }),
    { addShown: true, addDisabled: false, reason: null, sectionShown: true },
    'possible',
  );
});

Deno.test('credential section: nothing before the first snapshot', () => {
  eq(credentialSectionPaint(null).addShown, false, 'no snapshot');
  eq(credentialSectionPaint(null).sectionShown, false, 'no snapshot');
});

// A stand-in for the shared list the wasm `importSourceKinds()` returns: the
// filter keeps its order and touches only `Outlook`.
const KINDS = ['KindA', 'Outlook', 'KindB'];

Deno.test('import source picker: an absent Outlook answer leaves Outlook out', () => {
  eq(importSourceKindsOffered(KINDS, { state: 'absent' }), ['KindA', 'KindB'], 'absent');
});

Deno.test('import source picker: a blocked or possible Outlook answer keeps it', () => {
  const blocked: Possibility = { state: 'blocked', reason: { key: 'mail_import.microsoft_unavailable' } };
  eq(importSourceKindsOffered(KINDS, blocked), KINDS, 'blocked');
  eq(importSourceKindsOffered(KINDS, { state: 'possible' }), KINDS, 'possible');
  eq(importSourceKindsOffered(KINDS, null), KINDS, 'not yet known');
});

Deno.test('create controls: blocked waiting before the first snapshot, never live', () => {
  eq(paintAnswer(checking), { shown: true, disabled: true, reason: checking.reason }, 'checking');
  eq(paintAnswer(null), { shown: true, disabled: true, reason: null }, 'wasm not loaded');
});

Deno.test('create controls: blocked with the domain reason, live once possible', () => {
  eq(paintAnswer(noDomain), { shown: true, disabled: true, reason: noDomain.reason }, 'no domain');
  eq(paintAnswer({ state: 'possible' }), { shown: true, disabled: false, reason: null }, 'possible');
  eq(paintAnswer({ state: 'absent' }).shown, false, 'absent');
});

Deno.test('a handler refuses on its control answer, with the reason when blocked', () => {
  eq(refusal({ state: 'possible' }), { refused: false }, 'possible goes on');
  const offline: Possibility = { state: 'blocked', reason: { key: 'offline.needs_nest' } };
  eq(refusal(offline), { refused: true, reason: offline.reason }, 'blocked refuses with reason');
  eq(refusal({ state: 'absent' }), { refused: true, reason: null }, 'absent refuses');
  eq(refusal(null), { refused: true, reason: null }, 'unknown refuses');
});

Deno.test('a gesture with no machine refuses with the unbuilt answer\'s reason', () => {
  const unbuilt: Possibility = { state: 'blocked', reason: { key: 'mail_settings.usable_unbuilt' } };
  eq(blockedReason(unbuilt), unbuilt.reason, 'blocked carries its reason');
  eq(blockedReason({ state: 'possible' }), null, 'possible has none');
  eq(blockedReason({ state: 'absent' }), null, 'absent has none');
});
