// Deno test for a form submit's paint and refusal (`form-gate.ts`;
// `docs/goal/ui/README.md` § Offer only what is possible, rule 4's form case).
// Run via:
//
//     deno test --allow-read --no-check apps/fauna-web/src/lib/form-gate.test.ts
//
// The predicates themselves are shared Rust and pinned there
// (`libs/fauna-feed/src/forms.rs`, …); `blankIs` below stands in for one so
// these tests exercise web's own part — paint and refuse from one answer.

import { askPossibility, formGate, formRefusal, type Possibility } from './form-gate.ts';
import { t } from './i18n/strings.ts';

function eq<T>(actual: T, expected: T, msg: string) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${msg}\n  expected: ${e}\n  actual:   ${a}`);
}

/** A stand-in predicate: blank draft → blocked with `key`, else possible. */
function blankIs(key: string): (draft: string) => Possibility {
  return (draft) =>
    draft.trim() === '' ? { state: 'blocked', reason: { key } } : { state: 'possible' };
}

const post = blankIs('feed.compose_empty');
const reply = blankIs('feed.reply_empty');

Deno.test('a blank draft paints the submit disabled with the resolved reason', () => {
  eq(formGate(post('   ')), { shown: true, disabled: true, reason: t.feed.compose_empty }, 'blank post');
  eq(formGate(reply('')), { shown: true, disabled: true, reason: t.feed.reply_empty }, 'blank reply');
});

Deno.test('a typed draft paints the submit live, with no reason', () => {
  eq(formGate(post('hello')), { shown: true, disabled: false, reason: '' }, 'typed post');
});

Deno.test('an absent answer is not shown at all', () => {
  eq(formGate({ state: 'absent' }).shown, false, 'absent hidden');
});

Deno.test('an unreadable answer (wasm not up) is disabled with no reason', () => {
  const answer = askPossibility(() => {
    throw new Error('WASM not initialized');
  });
  eq(answer, null, 'a throwing ask reads as null');
  eq(formGate(answer), { shown: true, disabled: true, reason: '' }, 'null paint');
});

Deno.test('the submit refuses a blocked draft with the same reason it painted', () => {
  const answer = askPossibility(() => post(''));
  eq(formRefusal(answer), formGate(answer).reason, 'refusal = painted reason');
  eq(formRefusal(answer), t.feed.compose_empty, 'refusal text');
});

Deno.test('the submit goes on when the draft is possible', () => {
  eq(formRefusal(askPossibility(() => post('hi'))), null, 'no refusal');
});

Deno.test('an unreadable answer refuses with still-loading, never a silent drop', () => {
  eq(formRefusal(null), t.common.still_loading, 'still loading');
});

Deno.test('an absent answer refuses with nothing to add', () => {
  eq(formRefusal({ state: 'absent' }), '', 'absent refuses');
});
